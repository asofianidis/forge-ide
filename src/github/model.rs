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
    pub milestone: Option<Milestone>,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Milestone {
    pub title: String,
    #[serde(deserialize_with = "null_default")]
    pub number: u64,
}

/// A label defined in the repository (`gh label list`).
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RepoLabel {
    pub name: String,
    pub color: String,
    #[serde(deserialize_with = "null_default")]
    pub description: String,
}

pub const DETAIL_FIELDS: &str = "number,title,body,author,state,isDraft,url,headRefName,\
baseRefName,headRefOid,createdAt,updatedAt,mergedAt,mergeable,mergeStateStatus,reviewDecision,\
additions,deletions,changedFiles,commits,comments,reviews,labels,statusCheckRollup,\
reviewRequests,assignees,files,isCrossRepository,milestone";

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
    /// First line of a multi-line comment.
    pub start_line: Option<u32>,
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

/// Someone who could be asked to review: a repository contributor and/or a
/// user GitHub lists as assignable (has access to the repository).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Person {
    pub login: String,
    pub name: String,
    /// Commits to the default branch (0 if not a contributor).
    pub contributions: u64,
    /// Has access to the repository, so a review request will succeed.
    pub has_access: bool,
}

impl Person {
    /// Secondary text for the picker: real name and why they're listed.
    pub fn detail(&self) -> String {
        let why = match (self.contributions, self.has_access) {
            (0, _) => "has access".to_owned(),
            (1, _) => "1 commit".to_owned(),
            (n, _) => format!("{n} commits"),
        };
        if self.name.is_empty() {
            why
        } else {
            format!("{} · {why}", self.name)
        }
    }
}

/// A row of `GET repos/{owner}/{repo}/contributors`.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Contributor {
    pub login: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub contributions: u64,
}

/// GraphQL query for users who can be requested as reviewers.
pub const ASSIGNABLE_QUERY: &str = "query($owner:String!,$repo:String!){repository(owner:$owner,\
name:$repo){assignableUsers(first:100){nodes{login name}}}}";

/// Extracts `(login, name)` pairs from the [`ASSIGNABLE_QUERY`] response.
pub fn parse_assignable(json: &str) -> Vec<(String, String)> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };
    v.pointer("/data/repository/assignableUsers/nodes")
        .and_then(|n| n.as_array())
        .map(|nodes| {
            nodes
                .iter()
                .filter_map(|n| {
                    let login = n.get("login")?.as_str()?.to_owned();
                    let name = n
                        .get("name")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_owned();
                    Some((login, name))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Merges assignable users and contributors (bots dropped), most active
/// contributors first.
pub fn merge_people(
    assignable: Vec<(String, String)>,
    contributors: Vec<Contributor>,
) -> Vec<Person> {
    let mut people: Vec<Person> = assignable
        .into_iter()
        .map(|(login, name)| Person {
            login,
            name,
            contributions: 0,
            has_access: true,
        })
        .collect();
    for c in contributors {
        if c.kind == "Bot" || c.login.is_empty() {
            continue;
        }
        match people
            .iter_mut()
            .find(|p| p.login.eq_ignore_ascii_case(&c.login))
        {
            Some(p) => p.contributions = c.contributions,
            None => people.push(Person {
                login: c.login,
                name: String::new(),
                contributions: c.contributions,
                has_access: false,
            }),
        }
    }
    people.sort_by(|a, b| {
        b.contributions
            .cmp(&a.contributions)
            .then_with(|| a.login.to_lowercase().cmp(&b.login.to_lowercase()))
    });
    people
}

/// People matching `query` (login or name, case-insensitive), skipping
/// `exclude`d logins. Login prefix matches rank first.
pub fn search_people<'a>(people: &'a [Person], query: &str, exclude: &[&str]) -> Vec<&'a Person> {
    let q = query.trim().trim_start_matches('@').to_lowercase();
    let mut hits: Vec<(u8, &Person)> = people
        .iter()
        .filter(|p| !exclude.iter().any(|e| e.eq_ignore_ascii_case(&p.login)))
        .filter_map(|p| {
            let login = p.login.to_lowercase();
            let rank = if q.is_empty() || login.starts_with(&q) {
                0
            } else if login.contains(&q) {
                1
            } else if p.name.to_lowercase().contains(&q) {
                2
            } else {
                return None;
            };
            Some((rank, p))
        })
        .collect();
    // Stable sort keeps the contribution order within a rank.
    hits.sort_by_key(|(rank, _)| *rank);
    hits.into_iter().map(|(_, p)| p).collect()
}

/// Thread ids, resolved state and per-file "viewed" state for a PR, which
/// only the GraphQL API exposes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrExtra {
    /// GraphQL node id of the pull request (for mutations).
    pub pr_id: String,
    /// Root comment database id → (thread node id, resolved).
    pub threads: std::collections::HashMap<u64, (String, bool)>,
    /// Paths the signed-in user marked as viewed.
    pub viewed: std::collections::HashSet<String>,
}

pub const EXTRA_QUERY: &str = "query($owner:String!,$repo:String!,$n:Int!){repository(owner:$owner,\
name:$repo){pullRequest(number:$n){id reviewThreads(first:100){nodes{id isResolved \
comments(first:1){nodes{databaseId}}}} files(first:100){nodes{path viewerViewedState}}}}}";

pub fn parse_pr_extra(json: &str) -> Result<PrExtra, String> {
    let v: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("unexpected response from gh: {e}"))?;
    let pr = v
        .pointer("/data/repository/pullRequest")
        .ok_or("pull request not found")?;
    let mut extra = PrExtra {
        pr_id: pr
            .get("id")
            .and_then(|i| i.as_str())
            .unwrap_or_default()
            .to_owned(),
        ..PrExtra::default()
    };
    let nodes = |path: &str| {
        pr.pointer(path)
            .and_then(|n| n.as_array())
            .cloned()
            .unwrap_or_default()
    };
    for t in nodes("/reviewThreads/nodes") {
        let id = t.get("id").and_then(|i| i.as_str()).unwrap_or_default();
        let resolved = t
            .get("isResolved")
            .and_then(|r| r.as_bool())
            .unwrap_or(false);
        if let Some(db) = t
            .pointer("/comments/nodes/0/databaseId")
            .and_then(|d| d.as_u64())
        {
            extra.threads.insert(db, (id.to_owned(), resolved));
        }
    }
    for f in nodes("/files/nodes") {
        if f.get("viewerViewedState").and_then(|s| s.as_str()) == Some("VIEWED")
            && let Some(p) = f.get("path").and_then(|p| p.as_str())
        {
            extra.viewed.insert(p.to_owned());
        }
    }
    Ok(extra)
}

/// A GitHub notification thread (`GET /notifications`).
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Notification {
    pub id: String,
    /// review_requested, comment, mention, author, ci_activity, …
    pub reason: String,
    pub unread: bool,
    pub updated_at: String,
    pub subject: NotificationSubject,
    pub repository: NotificationRepo,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct NotificationSubject {
    pub title: String,
    #[serde(deserialize_with = "null_default")]
    pub url: String,
    /// PullRequest, Issue, CheckSuite, …
    #[serde(rename = "type")]
    pub kind: String,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct NotificationRepo {
    pub full_name: String,
}

impl Notification {
    pub fn is_pull_request(&self) -> bool {
        self.subject.kind == "PullRequest"
    }

    /// PR number from the subject URL (`…/pulls/42`).
    pub fn pr_number(&self) -> Option<u64> {
        let (rest, n) = self.subject.url.rsplit_once('/')?;
        rest.ends_with("/pulls").then(|| n.parse().ok()).flatten()
    }

    pub fn reason_label(&self) -> &str {
        match self.reason.as_str() {
            "review_requested" => "Review requested",
            "comment" => "New comment",
            "mention" | "team_mention" => "Mentioned",
            "author" => "On your PR",
            "assign" => "Assigned",
            "state_change" => "State changed",
            "ci_activity" => "CI",
            "subscribed" | "manual" => "Watching",
            other => other,
        }
    }
}

/// A GitHub Actions workflow run (`gh run list`).
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct WorkflowRun {
    pub database_id: u64,
    pub name: String,
    pub display_title: String,
    /// queued, in_progress, completed, …
    pub status: String,
    /// success, failure, cancelled, skipped, … (empty until completed)
    pub conclusion: String,
    pub workflow_name: String,
    pub head_sha: String,
    pub head_branch: String,
    pub url: String,
    pub event: String,
    pub created_at: String,
}

pub const RUN_FIELDS: &str = "databaseId,name,displayTitle,status,conclusion,workflowName,headSha,headBranch,url,event,createdAt";

impl WorkflowRun {
    pub fn outcome(&self) -> CheckOutcome {
        if self.status != "completed" {
            return CheckOutcome::Pending;
        }
        match self.conclusion.as_str() {
            "success" => CheckOutcome::Pass,
            "skipped" | "neutral" => CheckOutcome::Skipped,
            _ => CheckOutcome::Fail,
        }
    }
}

/// Run and job ids from a GitHub Actions check URL
/// (`…/actions/runs/{run}/job/{job}`).
pub fn actions_job(url: &str) -> Option<(u64, Option<u64>)> {
    let rest = url.split("/actions/runs/").nth(1)?;
    let mut parts = rest.split('/');
    let run = parts.next()?.parse().ok()?;
    let job = match (parts.next(), parts.next()) {
        (Some("job"), Some(j)) => j.split(['?', '#']).next()?.parse().ok(),
        _ => None,
    };
    Some((run, job))
}

/// The ```` ```suggestion ```` blocks in a review comment body, as
/// `(text before, suggested lines)` pieces plus trailing text.
pub fn split_suggestions(body: &str) -> (Vec<(String, String)>, String) {
    let mut parts = Vec::new();
    let mut rest = body;
    while let Some(start) = rest.find("```suggestion") {
        let before = &rest[..start];
        let after = &rest[start + "```suggestion".len()..];
        let after = after.split_once('\n').map_or("", |(_, a)| a);
        let Some(end) = after.find("```") else { break };
        let suggestion = after[..end].strip_suffix('\n').unwrap_or(&after[..end]);
        parts.push((before.to_owned(), suggestion.replace("\r\n", "\n")));
        rest = after[end + 3..].trim_start_matches(['\r', '\n']);
    }
    (parts, rest.to_owned())
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
    fn merges_and_searches_people() {
        let assignable = parse_assignable(
            r#"{"data":{"repository":{"assignableUsers":{"nodes":[
                {"login":"asofianidis","name":"Alex"},{"login":"MUDesigns","name":null}]}}}}"#,
        );
        let contributors = vec![
            Contributor {
                login: "asofianidis".into(),
                kind: "User".into(),
                contributions: 15,
            },
            Contributor {
                login: "dependabot[bot]".into(),
                kind: "Bot".into(),
                contributions: 9,
            },
            Contributor {
                login: "drive-by".into(),
                kind: "User".into(),
                contributions: 2,
            },
        ];
        let people = merge_people(assignable, contributors);
        let logins: Vec<_> = people.iter().map(|p| p.login.as_str()).collect();
        assert_eq!(logins, ["asofianidis", "drive-by", "MUDesigns"]);
        assert!(people[0].has_access && !people[1].has_access);
        assert_eq!(people[0].detail(), "Alex · 15 commits");
        assert_eq!(people[2].detail(), "has access");

        // The author is excluded; login prefix beats a name match.
        let hits = search_people(&people, "", &["asofianidis"]);
        assert_eq!(hits.len(), 2);
        let hits = search_people(&people, "@mu", &[]);
        assert_eq!(hits[0].login, "MUDesigns");
        let hits = search_people(&people, "alex", &[]);
        assert_eq!(hits[0].login, "asofianidis");
        assert!(search_people(&people, "zzz", &[]).is_empty());
        assert!(parse_assignable("not json").is_empty());
    }

    #[test]
    fn parses_extras_notifications_and_runs() {
        let extra = parse_pr_extra(
            r#"{"data":{"repository":{"pullRequest":{"id":"PR_1","reviewThreads":{"nodes":[
                {"id":"T1","isResolved":true,"comments":{"nodes":[{"databaseId":7}]}}]},
                "files":{"nodes":[{"path":"a.rs","viewerViewedState":"VIEWED"},
                {"path":"b.rs","viewerViewedState":"UNVIEWED"}]}}}}}"#,
        )
        .unwrap();
        assert_eq!(extra.pr_id, "PR_1");
        assert_eq!(extra.threads.get(&7), Some(&("T1".to_owned(), true)));
        assert!(extra.viewed.contains("a.rs") && !extra.viewed.contains("b.rs"));

        let n: Notification = serde_json::from_str(
            r#"{"id":"1","reason":"review_requested","unread":true,
                "subject":{"title":"T","url":"https://api.github.com/repos/o/r/pulls/42","type":"PullRequest"},
                "repository":{"full_name":"o/r"}}"#,
        )
        .unwrap();
        assert_eq!(n.pr_number(), Some(42));
        assert!(n.is_pull_request());
        let ci: Notification =
            serde_json::from_str(r#"{"subject":{"url":null,"type":"CheckSuite"}}"#).unwrap();
        assert_eq!(ci.pr_number(), None);

        assert_eq!(
            actions_job("https://github.com/o/r/actions/runs/123/job/456"),
            Some((123, Some(456)))
        );
        assert_eq!(
            actions_job("https://github.com/o/r/actions/runs/9"),
            Some((9, None))
        );
        assert_eq!(actions_job("https://example.com/x"), None);
    }

    #[test]
    fn splits_suggestion_blocks() {
        let (parts, tail) = split_suggestions("Use this:\n```suggestion\nlet x = 2;\n```\nthanks");
        assert_eq!(
            parts,
            vec![("Use this:\n".to_owned(), "let x = 2;".to_owned())]
        );
        assert_eq!(tail, "thanks");
        let (parts, tail) = split_suggestions("no suggestion");
        assert!(parts.is_empty());
        assert_eq!(tail, "no suggestion");
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
