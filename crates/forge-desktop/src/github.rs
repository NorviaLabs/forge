//! Read-only GitHub views; reuse the terminal's bounded `gh` adapter.
use forge_workspace::{gh, github};
use serde_json::{json, Value};
use std::path::Path;

pub fn issues(root: &Path) -> Result<Vec<Value>, String> {
    github::list_open_issues(root, 100)
        .map(|issues| {
            issues.into_iter().map(|issue| json!({
        "number": issue.number, "title": issue.title, "state": issue.state,
        "labels": issue.labels.into_iter().map(|label| label.name).collect::<Vec<_>>()
    })).collect()
        })
        .map_err(|error| error.to_string())
}

pub fn details(
    root: &Path,
    issue_number: Option<u64>,
    pr_number: Option<u64>,
) -> Result<Value, String> {
    if !matches!(
        (issue_number, pr_number),
        (Some(1..), None) | (None, Some(1..))
    ) {
        return Err("Choose one positive issue or pull request number".into());
    }
    let mut issue = Value::Null;
    let pr = if let Some(number) = issue_number {
        let details =
            gh::issue_details(root, &number.to_string()).map_err(|error| error.to_string())?;
        issue = json!({ "number": number, "title": details.issue.title, "body": details.body, "state": details.issue.state });
        gh::find_issue_pull_request(root, number).map_err(|error| error.to_string())?
    } else {
        Some(
            gh::pull_request(root, &pr_number.unwrap().to_string())
                .map_err(|error| error.to_string())?,
        )
    };
    let mut checks = Vec::new();
    let mut checks_error = String::new();
    let pull_request = if let Some(pr) = pr {
        match gh::check_details(root, &pr.number.to_string()) {
            Ok(rows) => checks = rows.into_iter().map(|row| json!({ "name": row.name, "state": row.state, "bucket": row.bucket, "link": row.link, "workflow": row.workflow })).collect(),
            Err(error) => checks_error = error.to_string(),
        }
        let after =
            gh::pull_request(root, &pr.number.to_string()).map_err(|error| error.to_string())?;
        if pr.head_sha != after.head_sha {
            return Err("PR head changed while loading checks; refresh again".into());
        }
        json!({ "number": pr.number, "title": pr.title, "state": pr.state, "draft": pr.is_draft, "review": pr.review_decision, "head": pr.head_branch, "base": pr.base_branch })
    } else {
        Value::Null
    };
    Ok(
        json!({ "issue": issue, "pr": pull_request, "checks": checks, "checks_error": checks_error }),
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn invalid_references_are_refused_before_running_gh() {
        for references in [
            (None, None),
            (Some(0), None),
            (Some(1), Some(2)),
            (None, Some(0)),
        ] {
            assert!(
                super::details(std::path::Path::new("/missing"), references.0, references.1)
                    .unwrap_err()
                    .contains("positive")
            );
        }
    }
}
