//! Check runs: the App's check by (head, name), created when absent, and updated outright or
//! by compare-and-set on its `external_id` (02 §5, T7).

use assay_app::{
    CheckConclusion, CheckFound, CheckName, CheckRun, CheckRunId, CheckState, CheckStatus,
    CheckUpdate, CommitId, CompareAndSetError, Deadline, LockedWrite,
};
use serde_json::{Map, Value, json};

use super::GitHub;
use super::api::{Failed, PER_PAGE, decode, encode, percent_encode};
use super::dto::{self, CheckRunDto, CheckRunsPage};
use super::error::{ForgeError, Operation};
use super::transport::{ApiPath, Request, Sleep, Transport};

impl super::api::Page for CheckRunsPage {
    type Item = CheckRunDto;

    fn items(self) -> Vec<CheckRunDto> {
        self.check_runs
    }
}

impl<T: Transport, S: Sleep> GitHub<T, S> {
    /// The check runs on `head` of every app, named `name` when given, stopping once `stop_at`
    /// are read.
    pub(crate) fn list_checks(
        &self,
        head: &CommitId,
        name: Option<&CheckName>,
        stop_at: Option<usize>,
    ) -> Result<Vec<CheckRunDto>, ForgeError<T::Error>> {
        let path = self.checks_path(head, name)?;
        self.api
            .list::<CheckRunsPage>(Operation::ListChecks, path, stop_at)
    }

    /// The listing of the check runs on `head`, named `name` when given.
    fn checks_path<E>(
        &self,
        head: &CommitId,
        name: Option<&CheckName>,
    ) -> Result<ApiPath, ForgeError<E>> {
        let mut rest = format!("commits/{head}/check-runs?filter=all&per_page={PER_PAGE}");
        if let Some(name) = name {
            rest.push_str("&check_name=");
            rest.push_str(&percent_encode(name.as_str()));
        }
        self.path(Operation::ListChecks, &rest)
    }

    /// The App's check named `name` on `head`. Same-named checks of other apps are skipped.
    /// Nothing is sent that would not end before `deadline`, when there is one.
    ///
    /// # Errors
    ///
    /// [`ForgeError::Ambiguous`] when the App has two; the listing's errors.
    pub(crate) fn find_app_check(
        &self,
        head: &CommitId,
        name: &CheckName,
        deadline: Option<Deadline>,
    ) -> Result<Option<CheckRun>, ForgeError<T::Error>> {
        let operation = Operation::ListChecks;
        let mut found = None;
        let path = self.checks_path(head, Some(name))?;
        let listed = self
            .api
            .bounded(deadline)
            .list::<CheckRunsPage>(operation, path, None)?;
        for check in listed {
            if dto::app(operation, check.app.as_ref())? != self.identity.app
                || check.name != name.as_str()
                || check.head_sha != head.as_str()
            {
                continue;
            }
            if found.is_some() {
                return Err(ForgeError::Ambiguous { operation });
            }
            found = Some(self.own_check(operation, check)?);
        }
        Ok(found)
    }

    /// Check `id` as GitHub has it now.
    fn read_check(
        &self,
        id: CheckRunId,
        deadline: Option<Deadline>,
    ) -> Result<CheckRunDto, ForgeError<T::Error>> {
        let operation = Operation::ReadCheck;
        let path = self.path(operation, &format!("check-runs/{}", id.0))?;
        self.api.bounded(deadline).get(operation, &path)
    }

    /// Overwrites check `id` with `update`; a PATCH replaces the same fields each time, so it is
    /// retried as is, though never past `deadline`, when there is one.
    pub(crate) fn patch_check(
        &self,
        id: CheckRunId,
        update: &CheckUpdate,
        deadline: Option<Deadline>,
    ) -> Result<(), ForgeError<T::Error>> {
        let operation = Operation::UpdateCheck;
        let body = encode(operation, &check_body(update, None))?;
        let path = self.path(operation, &format!("check-runs/{}", id.0))?;
        self.api
            .bounded(deadline)
            .send(operation, Request::patch(&path, &body))
            .map(drop)
    }

    /// Writes `write.update` to check `id` only if it is the App's and still carries
    /// `expected`, sending nothing that would not end before `write.deadline`.
    pub(crate) fn compare_and_set(
        &self,
        id: CheckRunId,
        expected: &str,
        write: LockedWrite<'_>,
    ) -> Result<(), CompareAndSetError<ForgeError<T::Error>>> {
        let LockedWrite { update, deadline } = write;
        let deadline = Some(deadline);
        // An empty stamp would match a check whose `external_id` was never set.
        if expected.is_empty() {
            return Err(CompareAndSetError::Forge(ForgeError::InvalidRequest {
                operation: Operation::UpdateCheck,
                detail: "the expected external_id is empty",
            }));
        }
        let current = self
            .read_check(id, deadline)
            .map_err(CompareAndSetError::Forge)?;
        let app = dto::app(Operation::ReadCheck, current.app.as_ref())
            .map_err(CompareAndSetError::Forge)?;
        if app != self.identity.app {
            return Err(CompareAndSetError::Forge(ForgeError::ForeignCheck { id }));
        }
        // GitHub reports an unset `external_id` as `null` or `""`; neither equals a stamp.
        let found = current.external_id.unwrap_or_default();
        if found != expected {
            return Err(CompareAndSetError::Conflict { found });
        }
        self.patch_check(id, update, deadline)
            .map_err(CompareAndSetError::Forge)
    }

    /// The App's check named `name` on `head`, created from `initial.update` when absent,
    /// sending nothing that would not end before `initial.deadline`.
    pub(crate) fn find_or_create(
        &self,
        head: &CommitId,
        name: &CheckName,
        initial: LockedWrite<'_>,
    ) -> Result<CheckFound, ForgeError<T::Error>> {
        let creation = Creation {
            update: initial.update,
            lookup: Lookup::First,
            deadline: Some(initial.deadline),
        };
        self.create(CheckTarget { head, name }, &creation)
    }

    /// Posts check `name` on `head`: a per-gate check, which the caller looked for first.
    pub(crate) fn create_check(
        &self,
        head: &CommitId,
        name: &CheckName,
        update: &CheckUpdate,
    ) -> Result<(), ForgeError<T::Error>> {
        let creation = Creation {
            update,
            lookup: Lookup::BeforeRetry,
            deadline: None,
        };
        self.create(CheckTarget { head, name }, &creation).map(drop)
    }

    /// Creates `target` from `creation.update`, looking for the App's check first when
    /// `creation.lookup` says so.
    ///
    /// A POST that failed retryably may still have created the check, so before each retry the
    /// list is read again: a check found then is the one this call created, never a duplicate.
    /// Under `creation.deadline`, no request is sent and no retry waited for that would not end
    /// before it.
    fn create(
        &self,
        target: CheckTarget<'_>,
        creation: &Creation<'_>,
    ) -> Result<CheckFound, ForgeError<T::Error>> {
        let &Creation {
            update,
            lookup,
            deadline,
        } = creation;
        let CheckTarget { head, name } = target;
        let operation = Operation::CreateCheck;
        let body = encode(operation, &check_body(update, Some(target)))?;
        let path = self.path(operation, "check-runs")?;
        let request = Request::post(&path, &body);
        let api = self.api.bounded(deadline);
        api.retrying(operation, |attempt| {
            let first = attempt == 0;
            let look = !first || lookup == Lookup::First;
            if look && let Some(found) = self.find_app_check(head, name, deadline)? {
                return Ok(if first {
                    CheckFound::Found(found)
                } else {
                    CheckFound::Created(found)
                });
            }
            let response = api.attempt(operation, &request)?;
            let created: CheckRunDto = decode(operation, &response.body)?;
            if created.name != name.as_str() || created.head_sha != head.as_str() {
                return Err(Failed::Stop(ForgeError::Malformed {
                    operation,
                    detail: "the created check has another name or head",
                }));
            }
            Ok(CheckFound::Created(self.own_check(operation, created)?))
        })
    }

    /// `check`, which must be the App's, as the app's [`CheckRun`].
    fn own_check<E>(
        &self,
        operation: Operation,
        check: CheckRunDto,
    ) -> Result<CheckRun, ForgeError<E>> {
        if dto::app(operation, check.app.as_ref())? != self.identity.app {
            return Err(ForgeError::ForeignCheck {
                id: CheckRunId(dto::nonzero(operation, check.id, "a check id is zero")?),
            });
        }
        let external_id = check
            .external_id
            .filter(|external_id| !external_id.is_empty())
            .ok_or(ForgeError::Malformed {
                operation,
                detail: "the App's check has no external_id",
            })?;
        Ok(CheckRun {
            id: CheckRunId(dto::nonzero(operation, check.id, "a check id is zero")?),
            external_id,
            status: check_status(operation, &check.status)?,
        })
    }
}

/// A check run's `status`, of the three the App's own checks can have.
pub(crate) fn check_status<E>(
    operation: Operation,
    status: &str,
) -> Result<CheckStatus, ForgeError<E>> {
    match status {
        "queued" => Ok(CheckStatus::Queued),
        "in_progress" => Ok(CheckStatus::InProgress),
        "completed" => Ok(CheckStatus::Completed),
        _ => Err(ForgeError::Malformed {
            operation,
            detail: "unknown check status",
        }),
    }
}

/// The JSON GitHub takes for `update`, with the check's name and head when creating it:
/// status, conclusion when completed, output and `external_id`. There is no `annotations`
/// field, so none ever accumulate (R3-11).
fn check_body(update: &CheckUpdate, target: Option<CheckTarget<'_>>) -> Value {
    let mut body = Map::new();
    if let Some(CheckTarget { head, name }) = target {
        body.insert("name".to_owned(), Value::from(name.as_str()));
        body.insert("head_sha".to_owned(), Value::from(head.as_str()));
    }
    match update.state {
        CheckState::InProgress => {
            body.insert("status".to_owned(), Value::from("in_progress"));
        }
        CheckState::Completed(conclusion) => {
            body.insert("status".to_owned(), Value::from("completed"));
            body.insert(
                "conclusion".to_owned(),
                Value::from(conclusion_code(conclusion)),
            );
        }
    }
    body.insert(
        "external_id".to_owned(),
        Value::from(update.external_id.as_str()),
    );
    body.insert(
        "output".to_owned(),
        json!({
            "title": update.title,
            "summary": update.summary,
            "text": update.text,
        }),
    );
    Value::Object(body)
}

/// The check a create is for: its head and name.
#[derive(Debug, Clone, Copy)]
struct CheckTarget<'a> {
    head: &'a CommitId,
    name: &'a CheckName,
}

/// How [`GitHub::create`] makes a check: from what, whether it looks first, and by when.
#[derive(Debug, Clone, Copy)]
struct Creation<'a> {
    /// The check's first state.
    update: &'a CheckUpdate,
    /// Whether to look for the App's check before the first POST.
    lookup: Lookup,
    /// What every request and retry wait must end before, under the publish lock.
    deadline: Option<Deadline>,
}

/// Whether [`GitHub::create`] looks for the App's check before its first POST.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lookup {
    /// Before the first POST and before each retry.
    First,
    /// Only before each retry: the caller already looked.
    BeforeRetry,
}

/// A conclusion's wire word.
const fn conclusion_code(conclusion: CheckConclusion) -> &'static str {
    match conclusion {
        CheckConclusion::Success => "success",
        CheckConclusion::Failure => "failure",
        CheckConclusion::Neutral => "neutral",
        CheckConclusion::Cancelled => "cancelled",
    }
}
