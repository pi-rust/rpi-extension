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


def enforce(repository, number):
    owner, name = repository.split("/", 1)
    pull = call_gh(PULL_REQUEST_QUERY, {"owner": owner, "name": name, "number": number})[
        "data"]["repository"]["pullRequest"]
    linked = [node["number"] for node in pull["closingIssuesReferences"]["nodes"]]

    def resolve(candidate):
        data = call_gh(ISSUE_QUERY, {"owner": owner, "name": name, "number": candidate})
        return data["data"]["repository"]["issueOrPullRequest"]

    found, tested = decide(candidates(linked, pull["body"]), resolve)
    if found is None:
        detail = "; ".join(tested) if tested else "the description has no issue reference"
        print(f"::error::Pull request #{number} must reference an existing issue first ({detail}).")
        print("Open an issue, then add `Closes #<issue>` to the pull request description.")
        message = f"Blocked: {detail}. Open an issue, then add `Closes #<issue>` to this description."
    else:
        message = f"Linked issue #{found['number']}: {found['title']} ({found['state']})"
        print(message)
    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary:
        with open(summary, "a", encoding="utf-8") as handle:
            handle.write(f"- {message}\n")
    return 0 if found else 1


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
