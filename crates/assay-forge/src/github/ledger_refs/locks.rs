//! The publish lock refs of the ledger repository, listed for the ledger-side reconcile
//! (06 §4, F21) with `GET git/matching-refs/heads/lock-`.

use assay_app::LockListing;

use super::super::api::PER_PAGE;
use super::super::error::{ForgeError, Operation};
use super::super::transport::{Sleep, Transport};
use super::LedgerRefsRepo;
use super::wire::RefView;

/// The prefix every listed ref must carry.
const LOCKS: &str = "refs/heads/lock-";

impl<T: Transport, S: Sleep> LockListing for LedgerRefsRepo<T, S> {
    /// A ref GitHub lists outside the prefix asked for is refused; the names are otherwise
    /// returned as listed, for `assay-app` to read.
    fn publish_locks(&self, limit: usize) -> Result<Vec<String>, Self::Error> {
        let operation = Operation::ListLedgerRefs;
        let path = self.repo_path(
            operation,
            &format!("git/matching-refs/heads/lock-?per_page={PER_PAGE}"),
        )?;
        let mut names = self
            .api
            .list::<Vec<RefView>>(operation, path, Some(limit))?
            .into_iter()
            .map(|found| {
                if found.name.starts_with(LOCKS) {
                    Ok(found.name)
                } else {
                    Err(ForgeError::Malformed {
                        operation,
                        detail: "a listed ref is not a publish lock",
                    })
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        names.truncate(limit);
        Ok(names)
    }
}
