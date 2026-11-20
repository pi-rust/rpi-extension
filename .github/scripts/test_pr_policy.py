"""Issue-first policy: pull requests must reference an existing issue."""
import importlib.util
from pathlib import Path
import subprocess
import unittest
from unittest import mock

spec = importlib.util.spec_from_file_location("pr_policy", Path(__file__).with_name("pr-policy.py"))
policy = importlib.util.module_from_spec(spec)
spec.loader.exec_module(policy)


class CandidateTests(unittest.TestCase):
    def test_linked_issues_come_first_then_closing_then_references(self):
        body = "Refs #40 and #12.\n\nCloses #7\nfixes #8, resolves #9"
        self.assertEqual(policy.candidates([3], body), [3, 7, 8, 9, 40, 12])

    def test_missing_or_empty_body_yields_no_candidates(self):
        for body in [None, "", "no references here"]:
            self.assertEqual(policy.candidates([], body), [])

    def test_issue_numbers_inside_words_or_urls_are_ignored(self):
        self.assertEqual(policy.candidates([], "see https://example.com/x#5 and abc#6"), [])
        self.assertEqual(policy.candidates([], "tag #11"), [11])


class DecideTests(unittest.TestCase):
    def resolve_map(self, mapping):
        return lambda number: mapping.get(number)

    def test_existing_issue_passes(self):
        found, tested = policy.decide([7], self.resolve_map({7: {"__typename": "Issue", "number": 7, "title": "t", "state": "OPEN"}}))
        self.assertEqual(found["number"], 7)
        self.assertEqual(tested, [])

    def test_pull_request_reference_is_rejected(self):
        found, tested = policy.decide([5], self.resolve_map({5: {"__typename": "PullRequest"}}))
        self.assertIsNone(found)
        self.assertIn("#5 is a pull request, not an issue", tested)

    def test_missing_reference_is_rejected(self):
        found, tested = policy.decide([99], self.resolve_map({}))
        self.assertIsNone(found)
        self.assertIn("#99 does not exist", tested)

    def test_first_real_issue_wins_when_references_mix(self):
        found, tested = policy.decide([1, 2, 3], self.resolve_map({
            1: {"__typename": "PullRequest"}, 2: {"__typename": "Issue", "number": 2, "title": "t", "state": "CLOSED"}}))
        self.assertEqual(found["number"], 2)
        self.assertEqual(tested, ["#1 is a pull request, not an issue"])

    def test_closed_issue_still_counts(self):
        found, _ = policy.decide([4], self.resolve_map({4: {"__typename": "Issue", "number": 4, "title": "t", "state": "CLOSED"}}))
        self.assertEqual(found["state"], "CLOSED")


class EnforceTests(unittest.TestCase):
    def test_fails_without_issue_and_writes_nothing_to_stdout_summary(self):
        pull = {"pullRequest": {"number": 1, "body": "no link", "closingIssuesReferences": {"nodes": []}}}
        with mock.patch.object(policy, "call_gh", return_value={"data": {"repository": pull}}):
            self.assertEqual(policy.enforce("o/r", 1), 1)

    def test_passes_and_reports_linked_issue(self):
        pull = {"pullRequest": {"number": 1, "body": "Closes #9", "closingIssuesReferences": {"nodes": []}}}
        issue = {"data": {"repository": {"issueOrPullRequest": {"__typename": "Issue", "number": 9, "title": "Add policy", "state": "OPEN"}}}}
        with mock.patch.object(policy, "call_gh", side_effect=[{"data": {"repository": pull}}, issue]):
            with mock.patch("builtins.open", mock.mock_open()) as opened, mock.patch.dict("os.environ", {"GITHUB_STEP_SUMMARY": "summary.md"}):
                self.assertEqual(policy.enforce("o/r", 1), 0)
                opened.assert_called_once_with("summary.md", "a", encoding="utf-8")

    def test_gh_failure_is_not_silently_treated_as_missing_issue(self):
        failure = subprocess.CompletedProcess([], 1, "", "HTTP 502")
        with mock.patch.object(policy.subprocess, "run", return_value=failure):
            with self.assertRaisesRegex(RuntimeError, "502"):
                policy.call_gh("query", {})

    def test_graphql_variables_are_typed(self):
        with mock.patch.object(policy.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, "{}", "")) as run:
            policy.call_gh("query", {"owner": "o", "number": 3})
            args = run.call_args.args[0]
            self.assertIn("-f", args)
            self.assertIn("owner=o", args)
            self.assertIn("-F", args)
            self.assertIn("number=3", args)


if __name__ == "__main__":
    unittest.main()
