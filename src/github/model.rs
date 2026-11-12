//! Data returned by `gh` (`--json` output and REST API responses).
//!
//! Everything is `#[serde(default)]` so missing or null fields from older
//! `gh` versions or unusual PRs never fail the whole parse.

use serde::{Deserialize, Deserializer};

/// Treats JSON `null` like a missing field.
fn null_default<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct Actor {
    #[serde(deserialize_with = "null_default")]
    pub login: String,
    #[serde(deserialize_with = "null_default")]
    pub name: String,
    pub is_bot: bool,
}

impl Actor {
    pub fn display(&self) -> &str {
        if self.login.is_empty() {
            "ghost"
        } else {
            &self.login
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Label {
    pub name: String,
    pub color: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckOutcome {
    Pass,
    Fail,
    Pending,
    Skipped,
}

/// A check run (GitHub Actions etc.) or a commit status context.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct Check {
    #[serde(rename = "__typename")]
    pub typename: String,
    // CheckRun fields
    #[serde(deserialize_with = "null_default")]
    pub name: String,
    #[serde(deserialize_with = "null_default")]
    pub status: String,
    #[serde(deserialize_with = "null_default")]
    pub conclusion: String,
    #[serde(deserialize_with = "null_default")]
    pub details_url: String,
    #[serde(deserialize_with = "null_default")]
    pub workflow_name: String,
    // StatusContext fields
    #[serde(deserialize_with = "null_default")]
    pub context: String,
    #[serde(deserialize_with = "null_default")]
    pub state: String,
    #[serde(deserialize_with = "null_default")]
    pub target_url: String,
    #[serde(deserialize_with = "null_default")]
    pub description: String,
}

impl Check {
    pub fn title(&self) -> String {
        let name = if self.name.is_empty() {
            &self.context
        } else {
            &self.name
        };
        if self.workflow_name.is_empty() {
            name.clone()
        } else {
            format!("{} / {name}", self.workflow_name)
        }
    }

    pub fn url(&self) -> &str {
        if self.details_url.is_empty() {
            &self.target_url
        } else {
            &self.details_url
        }
    }

    pub fn outcome(&self) -> CheckOutcome {
        if self.typename == "StatusContext" {
            return match self.state.as_str() {
                "SUCCESS" => CheckOutcome::Pass,
                "FAILURE" | "ERROR" => CheckOutcome::Fail,
                _ => CheckOutcome::Pending,
            };
        }
        if self.status != "COMPLETED" {
            return CheckOutcome::Pending;
        }
        match self.conclusion.as_str() {
            "SUCCESS" => CheckOutcome::Pass,
            "SKIPPED" | "NEUTRAL" => CheckOutcome::Skipped,
            _ => CheckOutcome::Fail,
        }
    }
}

/// Overall check state for a PR: any failure wins, then pending.
pub fn rollup(checks: &[Check]) -> Option<CheckOutcome> {
    if checks.is_empty() {
        return None;
    }
    let outcomes: Vec<_> = checks.iter().map(Check::outcome).collect();
    Some(if outcomes.contains(&CheckOutcome::Fail) {
        CheckOutcome::Fail
    } else if outcomes.contains(&CheckOutcome::Pending) {
        CheckOutcome::Pending
    } else {
        CheckOutcome::Pass
    })
}

/// One row of `gh pr list`.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct PrSummary {
    pub number: u64,
    pub title: String,
    pub author: Actor,
    pub head_ref_name: String,
    pub base_ref_name: String,
    pub is_draft: bool,
    /// OPEN, CLOSED or MERGED.
    pub state: String,
    /// APPROVED, CHANGES_REQUESTED, REVIEW_REQUIRED or empty.
    #[serde(deserialize_with = "null_default")]
    pub review_decision: String,
    pub created_at: String,
    pub updated_at: String,
    pub url: String,
    #[serde(deserialize_with = "null_default")]
    pub labels: Vec<Label>,
    pub additions: u64,
    pub deletions: u64,
    #[serde(deserialize_with = "null_default")]
    pub status_check_rollup: Vec<Check>,
}

pub const LIST_FIELDS: &str = "number,title,author,headRefName,baseRefName,isDraft,state,\
reviewDecision,createdAt,updatedAt,url,labels,additions,deletions,statusCheckRollup";

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct CommitAuthor {
    #[serde(deserialize_with = "null_default")]
    pub login: String,
    #[serde(deserialize_with = "null_default")]
    pub name: String,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct PrCommit {
    pub oid: String,
    pub message_headline: String,
    pub message_body: String,
    pub authored_date: String,
    #[serde(deserialize_with = "null_default")]
    pub authors: Vec<CommitAuthor>,
}

impl PrCommit {
    pub fn author(&self) -> &str {
        self.authors
            .first()
            .map(|a| {
                if a.login.is_empty() {
                    a.name.as_str()
                } else {
                    a.login.as_str()
                }
            })
            .unwrap_or("unknown")
    }
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct IssueComment {
    pub id: String,
    pub author: Actor,
    pub body: String,
    pub created_at: String,
    pub url: String,
    pub author_association: String,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct Review {
    pub id: String,
    pub author: Actor,
    pub body: String,
    /// APPROVED, CHANGES_REQUESTED, COMMENTED, DISMISSED, PENDING.
    pub state: String,
    #[serde(deserialize_with = "null_default")]
    pub submitted_at: String,
}

/// A requested reviewer: a user (`login`) or a team (`name`/`slug`).
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ReviewRequest {
    #[serde(deserialize_with = "null_default")]
    pub login: String,
    #[serde(deserialize_with = "null_default")]
    pub name: String,
    #[serde(deserialize_with = "null_default")]
    pub slug: String,
}

impl ReviewRequest {
    pub fn display(&self) -> &str {
        [&self.login, &self.slug, &self.name]
            .into_iter()
            .find(|s| !s.is_empty())
            .map_or("unknown", String::as_str)
    }
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct PrFile {
    pub path: String,
    pub additions: u64,
    pub deletions: u64,
}

/// Everything shown in the pull request view.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct PrDetail {
    pub number: u64,
    pub title: String,
    #[serde(deserialize_with = "null_default")]
    pub body: String,
    pub author: Actor,
    pub state: String,
    pub is_draft: bool,
    pub url: String,
    pub head_ref_name: String,
    pub base_ref_name: String,
    pub head_ref_oid: String,
    pub created_at: String,
    pub updated_at: String,
    #[serde(deserialize_with = "null_default")]
    pub merged_at: String,
    /// MERGEABLE, CONFLICTING or UNKNOWN.
    pub mergeable: String,
    /// CLEAN, BLOCKED, BEHIND, DIRTY, UNSTABLE, DRAFT, …
    pub merge_state_status: String,
    #[serde(deserialize_with = "null_default")]
    pub review_decision: String,
    pub additions: u64,
    pub deletions: u64,
    pub changed_files: u64,
    #[serde(deserialize_with = "null_default")]
    pub commits: Vec<PrCommit>,
    #[serde(deserialize_with = "null_default")]
    pub comments: Vec<IssueComment>,
    #[serde(deserialize_with = "null_default")]
    pub reviews: Vec<Review>,
    #[serde(deserialize_with = "null_default")]
    pub labels: Vec<Label>,
    #[serde(deserialize_with = "null_default")]
    pub status_check_rollup: Vec<Check>,
    #[serde(deserialize_with = "null_default")]
    pub review_requests: Vec<ReviewRequest>,
    #[serde(deserialize_with = "null_default")]
    pub assignees: Vec<Actor>,
    #[serde(deserialize_with = "null_default")]
    pub files: Vec<PrFile>,
    pub is_cross_repository: bool,
}

pub const DETAIL_FIELDS: &str = "number,title,body,author,state,isDraft,url,headRefName,\
baseRefName,headRefOid,createdAt,updatedAt,mergedAt,mergeable,mergeStateStatus,reviewDecision,\
additions,deletions,changedFiles,commits,comments,reviews,labels,statusCheckRollup,\
reviewRequests,assignees,files,isCrossRepository";

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RestUser {
    pub login: String,
}

/// An inline review comment (REST `pulls/{n}/comments`).
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ReviewComment {
    pub id: u64,
    pub body: String,
    pub path: String,
    /// Line in the current diff; `None` when the comment is outdated.
    pub line: Option<u32>,
    pub original_line: Option<u32>,
    /// "LEFT" or "RIGHT".
    #[serde(deserialize_with = "null_default")]
    pub side: String,
    pub user: RestUser,
    pub created_at: String,
    pub in_reply_to_id: Option<u64>,
    pub html_url: String,
    #[serde(deserialize_with = "null_default")]
    pub diff_hunk: String,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct DefaultBranch {
    pub name: String,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct RepoInfo {
    pub name_with_owner: String,
    pub url: String,
    #[serde(deserialize_with = "null_default")]
    pub default_branch_ref: DefaultBranch,
}

/// Parses one or more concatenated JSON arrays (`gh api --paginate`
/// prints one array per page) into a single list.
pub fn parse_paginated<T: for<'de> Deserialize<'de>>(text: &str) -> Result<Vec<T>, String> {
    let mut out = Vec::new();
    for page in serde_json::Deserializer::from_str(text).into_iter::<Vec<T>>() {
        out.extend(page.map_err(|e| format!("unexpected response from gh: {e}"))?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_detail_with_nulls_and_union_checks() {
        let json = r#"{"number":12,"title":"T","body":null,"author":{"login":"a","name":null},
            "state":"OPEN","reviewDecision":"","mergedAt":null,
            "statusCheckRollup":[
              {"__typename":"CheckRun","name":"build","status":"COMPLETED","conclusion":"SUCCESS","workflowName":"CI","detailsUrl":"u"},
              {"__typename":"CheckRun","name":"lint","status":"IN_PROGRESS","conclusion":""},
              {"__typename":"StatusContext","context":"ci/x","state":"FAILURE","targetUrl":"t"}],
            "reviewRequests":[{"__typename":"User","login":"bob"},{"__typename":"Team","name":"Core","slug":"core"}],
            "files":[{"path":"src/app.rs","additions":13,"deletions":6,"changeType":"MODIFIED"}]}"#;
        let pr: PrDetail = serde_json::from_str(json).unwrap();
        assert_eq!(pr.body, "");
        assert_eq!(pr.review_decision, "");
        let c = &pr.status_check_rollup;
        assert_eq!(c[0].outcome(), CheckOutcome::Pass);
        assert_eq!(c[0].title(), "CI / build");
        assert_eq!(c[1].outcome(), CheckOutcome::Pending);
        assert_eq!(c[2].outcome(), CheckOutcome::Fail);
        assert_eq!(c[2].url(), "t");
        assert_eq!(rollup(c), Some(CheckOutcome::Fail));
        assert_eq!(pr.review_requests[1].display(), "core");
        assert_eq!(pr.files[0].additions, 13);
    }

    #[test]
    fn parses_paginated_arrays() {
        let text = r#"[{"id":1,"path":"a","line":3,"side":"RIGHT","user":{"login":"x"}}]
                      [{"id":2,"path":"a","line":null,"in_reply_to_id":1,"user":{"login":"y"}}]"#;
        let comments: Vec<ReviewComment> = parse_paginated(text).unwrap();
        assert_eq!(comments.len(), 2);
        assert_eq!(comments[1].line, None);
        assert_eq!(comments[1].in_reply_to_id, Some(1));
        assert!(parse_paginated::<ReviewComment>("").unwrap().is_empty());
    }
}
