# Contributing / 参与贡献

`main` is protected: no one pushes directly, every change lands through a pull
request, and every pull request must reference an **existing issue** that was
opened first. The `require-issue` status check enforces the rule, and it runs
from `pull_request_target` so a pull request cannot weaken it.

`main` 受保护：任何人都不能直接推送，所有改动都通过 PR 合并；每个 PR 都必须先有
一个已存在的 **issue**。该规则由 `require-issue` 状态检查强制执行，它通过
`pull_request_target` 运行，PR 本身无法削弱检查逻辑。

## Workflow / 流程

1. **Issue first / 先提 issue.** Describe the problem or the change, including
   the affected package and the checks you expect. 描述问题或改动、涉及的包与预期检查。
2. **Branch and pull request / 建分支并提 PR.** Link the issue in the
   description with `Closes #<issue>`; plain `#<issue>` references also count,
   but must resolve to an issue (a linked pull request is rejected).
   在描述中用 `Closes #<issue>` 关联 issue；单独写 `#<issue>` 也有效，但必须能解析到
   issue（指向 PR 会被拒绝）。
3. **Checks / 检查.** `PR policy / require-issue` stays required. Add the test
   commands relevant to your change, for example:
   ```sh
   cargo test --workspace --locked
   python3 -m unittest discover -s .github/scripts -p 'test_*.py' -v
   ```
4. **Merge / 合并.** Squash merge. `main` stays linear enough for release tags
   to point at reviewed commits. 使用 squash 合并，保证发布标签指向已审阅的提交。

## Changing release behavior / 修改发布行为

Read [docs/github-releases.md](docs/github-releases.md) first. Release tags are
immutable, so a released package changes only by bumping its Cargo version and
re-running `python3 .github/scripts/sync-release-catalog.py`. 发布标签不可变，
已发布的包只能通过提升 Cargo 版本并重新同步目录来更新。

## Language / 语言

Documentation is bilingual: `README.md` (English) and `README.zh-CN.md`
(简体中文). Keep both in sync when user-facing behavior changes.
文档为中英双语，面向用户的行为变化时请同步更新两份 README。
