# SUI Full-Stack Project

## SUI Skill Routing
<!-- 完整路由表在 .claude/rules/skill-routing.md，由 claude-init 自動複製 -->

## Code Review（強制）

**Move 合約禁止使用 generic `superpowers:code-reviewer`。**

- Move code review → 用 `sui-code-review` skill（串接 move-code-quality → sui-security-guard → sui-red-team）
- Architecture review → 用 `sui-architect` skill
- SUI 前端 dApp review → `sui-frontend` + generic reviewer 可輔助
- 只有非 Move、非 SUI 整合的 code 才可單獨用 generic reviewer
