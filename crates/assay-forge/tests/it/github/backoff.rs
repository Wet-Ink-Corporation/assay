//! Backoff x3 on 5xx and secondary rate limits, then a typed publish-error (06 F8); every other
//! refusal is not retried.

use std::time::Duration;

use assay_app::Forge;
use assay_forge::github::{ForgeError, Method, Operation};
use serde_json::json;

use super::recorded::{Recorded, Waits, fixture, github, json, path, pr, reply};

/// The labels of PR 268, a read to drive the retry policy with.
fn labels() -> String {
    path("issues/268/labels?per_page=100")
}

/// `seconds` as waits.
fn seconds(seconds: &[u64]) -> Vec<Duration> {
    seconds.iter().copied().map(Duration::from_secs).collect()
}

/// A failure of `status` with no headers or body.
fn failure(status: u16) -> assay_forge::github::Response {
    reply(status, &[], Vec::new())
}

#[test]
fn a_5xx_is_retried_with_doubling_waits_until_it_succeeds() {
    let recorded = Recorded::default()
        .then(Method::Get, &labels(), failure(500))
        .then(Method::Get, &labels(), failure(502))
        .then(Method::Get, &labels(), failure(503))
        .ok(Method::Get, &labels(), fixture("labels.json"));
    let waits = Waits::default();

    let labels = github(&recorded, &waits).labels(pr());

    assert_eq!(
        labels.expect("the fourth attempt succeeds"),
        ["assay:recheck", "bug"]
    );
    assert_eq!(waits.taken(), seconds(&[1, 2, 4]));
}

#[test]
fn a_fourth_5xx_is_a_publish_error() {
    let recorded = (0..4).fold(Recorded::default(), |recorded, _| {
        recorded.then(Method::Get, &labels(), failure(502))
    });
    let waits = Waits::default();

    let labels = github(&recorded, &waits).labels(pr());

    let error = labels.expect_err("every attempt failed");
    assert!(
        matches!(
            error,
            ForgeError::Exhausted {
                operation: Operation::ListLabels,
                status: 502,
                attempts: 4
            }
        ),
        "{error:?}"
    );
    assert!(error.to_string().starts_with("publish-error: "), "{error}");
    assert_eq!(waits.taken(), seconds(&[1, 2, 4]), "three backoffs");
    assert_eq!(recorded.unused(), 0, "four requests");
}

#[test]
fn a_secondary_rate_limit_waits_as_long_as_github_asks() {
    let recorded = Recorded::default()
        .then(
            Method::Get,
            &labels(),
            reply(403, &[("Retry-After", "30")], Vec::new()),
        )
        .ok(Method::Get, &labels(), fixture("labels.json"));
    let waits = Waits::default();

    let labels = github(&recorded, &waits).labels(pr());

    assert_eq!(
        labels.expect("the retry succeeds"),
        ["assay:recheck", "bug"]
    );
    assert_eq!(waits.taken(), seconds(&[30]));
}

#[test]
fn a_secondary_rate_limit_message_backs_off_without_retry_after() {
    let message = json(&json!({
        "message": "You have exceeded a secondary rate limit. Please wait a few minutes before you try again.",
        "documentation_url": "https://docs.github.com/rest/overview/rate-limits-for-the-rest-api",
    }));
    let recorded = Recorded::default()
        .then(Method::Get, &labels(), reply(403, &[], message))
        .ok(Method::Get, &labels(), fixture("labels.json"));
    let waits = Waits::default();

    let labels = github(&recorded, &waits).labels(pr());

    assert_eq!(
        labels.expect("the retry succeeds"),
        ["assay:recheck", "bug"]
    );
    assert_eq!(waits.taken(), seconds(&[1]));
}

#[test]
fn a_permission_refusal_is_not_retried() {
    let message = json(&json!({
        "message": "Resource not accessible by integration",
        "status": "403",
    }));
    let recorded = Recorded::default().then(Method::Get, &labels(), reply(403, &[], message));
    let waits = Waits::default();

    let labels = github(&recorded, &waits).labels(pr());

    assert!(
        matches!(
            labels,
            Err(ForgeError::Rejected {
                operation: Operation::ListLabels,
                status: 403
            })
        ),
        "{labels:?}"
    );
    assert_eq!(waits.taken(), Vec::new(), "no wait");
}

#[test]
fn a_wait_beyond_a_minute_gives_up_at_once() {
    let recorded = Recorded::default().then(
        Method::Get,
        &labels(),
        reply(429, &[("retry-after", "3600")], Vec::new()),
    );
    let waits = Waits::default();

    let labels = github(&recorded, &waits).labels(pr());

    assert!(
        matches!(
            labels,
            Err(ForgeError::Exhausted {
                status: 429,
                attempts: 1,
                ..
            })
        ),
        "{labels:?}"
    );
    assert_eq!(waits.taken(), Vec::new(), "an hour is never waited");
}

#[test]
fn an_unreadable_retry_after_is_refused() {
    let recorded = Recorded::default().then(
        Method::Get,
        &labels(),
        reply(
            429,
            &[("Retry-After", "Wed, 21 Oct 2026 07:28:00 GMT")],
            Vec::new(),
        ),
    );
    let waits = Waits::default();

    let labels = github(&recorded, &waits).labels(pr());

    assert!(
        matches!(labels, Err(ForgeError::Malformed { .. })),
        "{labels:?}"
    );
}
