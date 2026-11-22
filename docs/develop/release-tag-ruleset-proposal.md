# GitHub 正式版本 tag 保护规则草案

> 状态：草案，供仓库管理员配置和核验；尚未应用到 GitHub。
> 适用仓库：`SpaceHuang/SpaceMemory`。

## 目的

正式版本 tag（例如 `v1.0.0`）一经创建，就应一直指向同一个提交。否则同一个下载链接或版本名可能在不同时间指向不同代码，构建来源、能力声明和问题复现都会失去对应关系。

规则只保护正式版本号 tag，不限制临时测试 tag、功能分支或普通开发提交。创建 `v1.0.0` 时仍可按正常发布流程推送；创建后，所有人都不能移动或删除它。

## 拟议规则

| 设置 | 拟议值 | 作用 |
|---|---|---|
| 名称 | `Protect release version tags` | 让仓库设置中易于识别 |
| 目标 | Tag | 规则作用于 Git tag |
| 匹配模式 | `v[0-9]*` | 覆盖 `v1.0.0`、`v2.3.4` 等版本 tag；不匹配 `test-v1.0.0` |
| 状态 | Active | 立即执行规则 |
| 禁止更新 | 开启 | tag 创建后不能改指向另一提交 |
| 禁止删除 | 开启 | tag 创建后不能删除 |
| 限制创建 | 关闭 | 允许发布流程首次创建正式版本 tag |
| 绕过者 | 无 | 管理员也不设日常绕过通道；若 GitHub 当前规则不支持空绕过列表，必须先说明实际绕过权限再配置 |

> 匹配模式需在 GitHub Rulesets 页面用 `fnmatch` 语法设置，并在保存后通过 API 读回确认。`v[0-9]*` 的意图是只匹配以 `v` 加数字开头的正式 tag；如 GitHub 对字符类或 glob 行为的实际解释与此不符，应改用明确的版本 tag 前缀模式并用 `v1.0.0`、`vNext`、`test-v1.0.0` 三个名字验证匹配范围。

## 可导入 API JSON 草案

这是创建 repository ruleset 的请求体，不包含服务器生成的 ID、时间或链接字段：

```json
{
  "name": "Protect release version tags",
  "target": "tag",
  "enforcement": "active",
  "bypass_actors": [],
  "conditions": {
    "ref_name": {
      "include": ["refs/tags/v[0-9]*"],
      "exclude": []
    }
  },
  "rules": [
    { "type": "update" },
    { "type": "deletion" }
  ]
}
```

对应 GitHub API 是 `POST /repos/SpaceHuang/SpaceMemory/rulesets`；应用这条规则需要 repository Administration 写权限。本文只拟定配置，没有调用写接口或修改远端设置。

## 配置与验收步骤

1. 仓库管理员在 **Settings → Rules → Rulesets → New tag ruleset** 创建上述规则；目标选 Tags，匹配正式版本 tag。
2. 保存后从 `GET /repos/SpaceHuang/SpaceMemory/rulesets` 及单条规则详情读回，记录规则 ID、活动状态、匹配模式、规则项和绕过者。
3. 确认规则只匹配正式版本 tag：`v1.0.0` 应匹配，`vNext` 与 `test-v1.0.0` 应不匹配。
4. 若需验证服务器实际拒绝更新/删除，先在一次性测试仓库建立相同规则，创建测试 tag，再确认更新与删除被拒；验证后删除整个测试仓库。不要在正式仓库用永久受保护的正式命名空间创建探针 tag，也不要拿正式版本 tag 做破坏性验证。
5. 归档正式仓库规则读回结果、一次性测试仓库的验证结果、验证日期与执行账号。
6. 创建正式版本 tag 后，再次读回规则和 tag 指向，保存 tag 名、提交 SHA、能力文件摘要及发布资产哈希。

## 安全边界

- 不通过创建后移动或删除正式 tag 来修复发布；修正内容必须发布新版本 tag。
- 若发现错误指向，应保留原 tag 和发布记录，停止分发该资产，按维护者批准的后续版本流程处理。
- 本规则只保证 tag 不被常规更新/删除；它不自动保证 tag 指向已审查提交、资产由可信工作流生成或二进制可复现。这些仍由发布审批、构建来源记录和资产校验负责。
- 管理员必须在实际配置界面/API 验证该计划的绕过语义及规则覆盖范围；草案本身不是已生效保护证明。

## 本仓库当前状态

截至 2026-11-22，远端只有一个针对 `refs/heads/main` 的 active ruleset，包含禁止删除和非快进更新；没有 tag ruleset。该分支规则不能保护 `v*` tag。本草案尚未应用，tag 保护验收仍待管理员配置并由只读查询确认。
