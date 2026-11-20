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
    def pull(self, body, login="writer", association="MEMBER"):
        return {"pullRequest": {"number": 1, "body": body, "author": {"login": login},
                                 "authorAssociation": association,
                                 "closingIssuesReferences": {"nodes": []}}}

    def test_writer_without_issue_passes(self):
        with mock.patch.object(policy, "call_gh", return_value={"data": {"repository": self.pull("no link")}}), \
                mock.patch.object(policy, "author_permission", return_value="write"), \
                mock.patch("builtins.open", mock.mock_open()) as opened, \
                mock.patch.dict("os.environ", {"GITHUB_STEP_SUMMARY": "summary.md"}):
            self.assertEqual(policy.enforce("o/r", 1), 0)
            opened.assert_called_once_with("summary.md", "a", encoding="utf-8")

    def test_admin_and_maintain_are_exempt_and_read_is_not(self):
        for permission, expected in [("admin", 0), ("maintain", 0), ("write", 0), ("triage", 1), ("read", 1), ("none", 1)]:
            with mock.patch.object(policy, "call_gh", return_value={"data": {"repository": self.pull("no link", login="someone")}}), \
                    mock.patch.object(policy, "author_permission", return_value=permission), \
                    mock.patch("builtins.open", mock.mock_open()), \
                    mock.patch.dict("os.environ", {}):
                self.assertEqual(policy.enforce("o/r", 1), expected, permission)

    def test_unreadable_permission_falls_back_to_author_association(self):
        for association, expected in [("MEMBER", 0), ("OWNER", 0), ("COLLABORATOR", 0), ("NONE", 1), ("CONTRIBUTOR", 1)]:
            with mock.patch.object(policy, "call_gh", return_value={"data": {"repository": self.pull("no link", association=association)}}), \
                    mock.patch.object(policy, "author_permission", return_value=None), \
                    mock.patch("builtins.open", mock.mock_open()), \
                    mock.patch.dict("os.environ", {}):
                self.assertEqual(policy.enforce("o/r", 1), expected, association)

    def test_bot_authored_release_pull_requests_are_exempt(self):
        for login in ["github-actions[bot]", "dependabot[bot]", "renovate[bot]"]:
            with mock.patch.object(policy, "call_gh", return_value={"data": {"repository": self.pull("no link", login=login, association="NONE")}}), \
                    mock.patch.object(policy, "author_permission") as permission, \
                    mock.patch.object(policy.subprocess, "run") as run, \
                    mock.patch("builtins.open", mock.mock_open()), \
                    mock.patch.dict("os.environ", {}):
                self.assertEqual(policy.enforce("o/r", 1), 0, login)
                permission.assert_not_called()
                run.assert_not_called()

    def test_graphql_bots_without_rest_suffix_are_exempt(self):
        for login in ["github-actions", "dependabot", "renovate"]:
            pull = self.pull("no link", login=login, association="NONE")
            pull["pullRequest"]["author"]["__typename"] = "Bot"
            with mock.patch.object(policy, "call_gh", return_value={"data": {"repository": pull}}), \
                    mock.patch.object(policy, "author_permission") as permission, \
                    mock.patch("builtins.open", mock.mock_open()):
                self.assertEqual(policy.enforce("o/r", 1), 0, login)
                permission.assert_not_called()

    def test_human_with_a_bot_like_login_still_needs_an_issue(self):
        pull = self.pull("no link", login="github-actions", association="NONE")
        pull["pullRequest"]["author"]["__typename"] = "User"
        with mock.patch.object(policy, "call_gh", return_value={"data": {"repository": pull}}), \
                mock.patch.object(policy, "author_permission", return_value="none"), \
                mock.patch("builtins.open", mock.mock_open()):
            self.assertEqual(policy.enforce("o/r", 1), 1)

    def test_query_requests_the_author_type(self):
        self.assertIn("author { __typename login }", policy.PULL_REQUEST_QUERY)

    def test_human_name_containing_bot_is_not_exempt(self):
        with mock.patch.object(policy, "call_gh", return_value={"data": {"repository": self.pull("no link", login="robotics", association="NONE")}}), \
                mock.patch.object(policy, "author_permission", return_value="none"), \
                mock.patch("builtins.open", mock.mock_open()), \
                mock.patch.dict("os.environ", {}):
            self.assertEqual(policy.enforce("o/r", 1), 1)

    def test_permission_lookup_distinguishes_missing_role_from_api_failure(self):
        cases = [
            (subprocess.CompletedProcess([], 0, "write\n", ""), "write"),
            (subprocess.CompletedProcess([], 1, "", "gh: Not Found (HTTP 404)"), "none"),
            (subprocess.CompletedProcess([], 1, "", "gh: Resource not accessible by integration (HTTP 403)"), None),
        ]
        for result, expected in cases:
            with mock.patch.object(policy.subprocess, "run", return_value=result):
                self.assertEqual(policy.author_permission("o/r", "someone"), expected)
        with mock.patch.object(policy.subprocess, "run", return_value=subprocess.CompletedProcess([], 1, "", "HTTP 502")):
            with self.assertRaises(RuntimeError):
                policy.author_permission("o/r", "someone")

    def test_fails_without_issue_and_writes_nothing_to_stdout_summary(self):
        with mock.patch.object(policy, "call_gh", return_value={"data": {"repository": self.pull("no link", login="outsider", association="NONE")}}), \
                mock.patch.object(policy, "author_permission", return_value="none"):
            self.assertEqual(policy.enforce("o/r", 1), 1)

    def test_passes_and_reports_linked_issue(self):
        pull = self.pull("Closes #9", login="outsider", association="NONE")
        issue = {"data": {"repository": {"issueOrPullRequest": {"__typename": "Issue", "number": 9, "title": "Add policy", "state": "OPEN"}}}}
        with mock.patch.object(policy, "call_gh", side_effect=[{"data": {"repository": pull}}, issue]), \
                mock.patch.object(policy, "author_permission", return_value="none"):
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
