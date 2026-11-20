"""Require a real, pre-existing issue before a pull request can merge.

The check runs from `pull_request_target`, so it always uses the base-branch
definition and never executes code from the pull request under review.
"""

import argparse
import json
import os
import re
import subprocess
import sys

CLOSING = re.compile(r"\b(?:close[sd]?|fix(?:e[sd])?|resolve[sd]?)\s*:?\s*#(\d+)", re.IGNORECASE)
REFERENCE = re.compile(r"(?<![\w#/])#(\d+)\b")

PULL_REQUEST_QUERY = """
query($owner: String!, $name: String!, $number: Int!) {
  repository(owner: $owner, name: $name) {
    pullRequest(number: $number) {
      number
      body
      author { login }
      authorAssociation
      closingIssuesReferences(first: 20) { nodes { number } }
    }
  }
}
"""

ISSUE_QUERY = """
query($owner: String!, $name: String!, $number: Int!) {
  repository(owner: $owner, name: $name) {
    issueOrPullRequest(number: $number) {
      __typename
      ... on Issue { number title state }
    }
  }
}
"""


# Roles that already maintain the repository do not need a tracking issue.
EXEMPT_PERMISSIONS = frozenset({"admin", "maintain", "write"})
# Used only when the workflow token cannot read collaborator permissions.
EXEMPT_ASSOCIATIONS = frozenset({"OWNER", "MEMBER", "COLLABORATOR"})


def candidates(linked, body):
    """Linked issues first, then explicit closing keywords, then plain #123."""
    text = body or ""
    ordered, seen = [], set()
    for number in list(linked) + [int(match) for match in CLOSING.findall(text)] + [
            int(match) for match in REFERENCE.findall(text)]:
        if number not in seen:
            seen.add(number)
            ordered.append(number)
    return ordered


def decide(numbers, resolve):
    """Return the first reference that resolves to an Issue, plus tried notes."""
    tested = []
    for number in numbers:
        found = resolve(number)
        if found is None:
            tested.append(f"#{number} does not exist")
            continue
        if found.get("__typename") != "Issue":
            tested.append(f"#{number} is a pull request, not an issue")
            continue
        return found, tested
    return None, tested


def call_gh(query, variables):
    """Run `gh api graphql` with typed variables (ints use -F, strings -f)."""
    args = ["gh", "api", "graphql", "-f", f"query={query}"]
    for key, value in variables.items():
        args += ["-F" if isinstance(value, int) else "-f", f"{key}={value}"]
    result = subprocess.run(args, capture_output=True, text=True)
    if result.returncode:
        raise RuntimeError(result.stderr.strip() or "gh api graphql failed")
    return json.loads(result.stdout)


def author_permission(repository, login):
    """Effective permission of the author, or None when the token cannot read it."""
    result = subprocess.run(
        ["gh", "api", f"repos/{repository}/collaborators/{login}/permission", "--jq", ".permission"],
        capture_output=True, text=True)
    if result.returncode == 0:
        return result.stdout.strip() or None
    stderr = result.stderr
    if "(HTTP 404)" in stderr or "Not Found" in stderr:
        return "none"
    if "(HTTP 403)" in stderr or "HTTP 401" in stderr or "Resource not accessible" in stderr:
        return None
    raise RuntimeError(stderr.strip() or "failed to read collaborator permission")


def exemption(repository, pull):
    """Return (exempt, reason) for maintainers; failures never grant exemption."""
    login = (pull.get("author") or {}).get("login")
    association = pull.get("authorAssociation") or "NONE"
    if not login:
        return False, f"unknown author ({association})"
    # GitHub Apps reserve the `[bot]` suffix and can only act through workflows,
    # so the automated release-plz version pull request must not need an issue.
    if login.endswith("[bot]"):
        return True, f"{login} is an automation account"
    permission = author_permission(repository, login)
    if permission is not None:
        return permission in EXEMPT_PERMISSIONS, f"{login} has the {permission} role"
    return association in EXEMPT_ASSOCIATIONS, f"{login} is {association} (permission unreadable)"


def enforce(repository, number):
    owner, name = repository.split("/", 1)
    pull = call_gh(PULL_REQUEST_QUERY, {"owner": owner, "name": name, "number": number})[
        "data"]["repository"]["pullRequest"]
    exempt, reason = exemption(repository, pull)
    if exempt:
        message = f"Exempt: {reason}; no issue link required."
        print(message)
        ok = True
    else:
        linked = [node["number"] for node in pull["closingIssuesReferences"]["nodes"]]

        def resolve(candidate):
            data = call_gh(ISSUE_QUERY, {"owner": owner, "name": name, "number": candidate})
            return data["data"]["repository"]["issueOrPullRequest"]

        found, tested = decide(candidates(linked, pull["body"]), resolve)
        if found is None:
            detail = "; ".join(tested) if tested else "the description has no issue reference"
            print(f"::error::Pull request #{number} must reference an existing issue first ({detail}; {reason}).")
            print("Open an issue, then add `Closes #<issue>` to the pull request description.")
            message = f"Blocked: {detail}; {reason}. Open an issue, then add `Closes #<issue>` to this description."
        else:
            message = f"Linked issue #{found['number']}: {found['title']} ({found['state']})"
            print(message)
        ok = found is not None
    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary:
        with open(summary, "a", encoding="utf-8") as handle:
            handle.write(f"- {message}\n")
    return 0 if ok else 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repository", default=os.environ.get("GITHUB_REPOSITORY"))
    parser.add_argument("--number", type=int)
    args = parser.parse_args()
    number = args.number
    if number is None:
        with open(os.environ["GITHUB_EVENT_PATH"], encoding="utf-8") as handle:
            number = json.load(handle)["pull_request"]["number"]
    if not args.repository or "/" not in args.repository:
        parser.error("--repository owner/repo is required")
    return enforce(args.repository, number)


if __name__ == "__main__":
    sys.exit(main())
