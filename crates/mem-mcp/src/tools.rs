//! MCP 工具面定义（与 CLI 命令面 §3.3 同构；命名定稿见开放问题29）。
//!
//! 工具响应附带修订令牌与预算截断标注（续读路径，红线9）。
//! schema 携带 `expect` 全令牌参数（版本 + 64 hex，红线19——禁截断摘要）。

use serde_json::json;

fn expect_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "description": "修订令牌 = v<版本>@<64位SHA-256 十六进制>（全摘要；截断摘要被拒绝，红线19）",
        "required": ["version"],
        "properties": {
            "version": { "type": "string", "pattern": "^v[0-9]+@[0-9a-f]{64}$" }
        }
    })
}

/// 工具定义（tools/list 返回）。
pub fn tool_definitions() -> serde_json::Value {
    json!([
        { "name": "mem_brief", "description": "冷启动工作视图。零写盘；无台账如实报告；直读标注。items 受 brief_items 条数与 brief_bytes 字节双层预算（超预算条目跳过，skipped_over_budget 清单可见；resume.offset 续读）",
          "inputSchema": { "type": "object", "properties": {
              "view": { "type": "string" }, "principal": { "type": "string" }, "since": { "type": "integer" },
              "offset": { "type": "integer", "description": "命中序号续读（取自上页 resume.offset）" } } } },
        { "name": "mem_session_start", "description": "显式建立/接手台账（幂等键 principal+view+task_ref；handoff=auto|<id>）",
          "inputSchema": { "type": "object", "required": ["principal"], "properties": {
              "principal": { "type": "string" }, "view": { "type": "string" },
              "task_ref": { "type": ["string", "null"] }, "handoff": { "type": "string" } } } },
        { "name": "mem_session_flush", "description": "提交读留痕，置 flushed（可接续）",
          "inputSchema": { "type": "object", "required": ["session"], "properties": {
              "session": { "type": "string" }, "trail": { "type": "array", "items": { "type": "string" } } } } },
        { "name": "mem_list", "description": "过滤定位（LIMIT+续读；返回完整令牌）。filters 载荷字段过滤可组合（值一律 JSON 字符串——标量文本，类型解释由字段声明承担，R10 决策④）；响应透出坏记录告警 warnings（决策⑦）",
          "inputSchema": { "type": "object", "properties": {
              "type": { "type": "string" }, "view": { "type": "string" },
              "filters": { "type": "object", "additionalProperties": { "type": "string" },
                  "description": "过滤条件 {字段: 标量文本}；值一律 JSON 字符串，非字符串标量被拒收（invalid_filter_value）" },
              "limit": { "type": "integer" }, "offset": { "type": "integer" } } } },
        { "name": "mem_find", "description": "关键词检索（词项与/与位排除 + 载荷字段过滤组合，R10）。query 必填且解析后须含至少一个肯定词项（空查询/纯否定拒收 query_invalid；纯过滤走 mem_list）。命中 = 定位信息 + 完整修订令牌；read_mode 标注快路径/直读；坏记录告警 warnings 透出（决策⑦）",
          "inputSchema": { "type": "object", "required": ["query"], "properties": {
              "query": { "type": "string", "description": "查询整串（空白分隔词项；- 前缀 = 否定词项）" },
              "filters": { "type": "object", "additionalProperties": { "type": "string" },
                  "description": "过滤条件 {字段: 标量文本}；值一律 JSON 字符串，非字符串标量被拒收（invalid_filter_value）" },
              "limit": { "type": "integer" },
              "offset": { "type": "integer", "description": "命中序号续读（取自上页 resume.offset）" } } } },
        { "name": "mem_show", "description": "钻取单条（返回信封/载荷/正文/令牌；零写盘）。正文按 brief_bytes 截断，body_offset 续读（R4.2/红线9）。正文区合法 UTF-8 → body 文本分页（切点码点对齐）；不合法 → body=null、body_b64 base64 分页（页=brief_bytes 个原始字节）；body_utf8 恒有供机判（R11/BD2）。body_b64 逐页独立编码（含各自 padding）——宿主必须逐页解码后按字节拼接，不得直接拼接各页 base64 文本",
          "inputSchema": { "type": "object", "required": ["id"], "properties": {
              "id": { "type": "string" },
              "body_offset": { "type": "integer", "description": "正文字节偏移续读（取自上页 resume.body_offset）" } } } },
        { "name": "mem_trace", "description": "多跳遍历（证据链/依赖者；预算+截断标注）。节点预算截断时 resume{skip,limit} 原样回传即可续读（不重复）",
          "inputSchema": { "type": "object", "required": ["id"], "properties": {
              "id": { "type": "string" }, "dir": { "type": "string", "enum": ["up", "down"] },
              "depth": { "type": "integer" },
              "skip": { "type": "integer", "description": "节点序号续读（取自上页 resume.skip）" },
              "limit": { "type": "integer", "description": "本页节点数上限（默认 trace_nodes，受全局预算钳制）" } } } },
        { "name": "mem_add", "description": "创建记录（唯一键路径独占；输者返回既有记录及令牌）。payload 必填（M4）",
          "inputSchema": { "type": "object", "required": ["type", "payload"], "properties": {
              "type": { "type": "string" }, "payload": { "type": "object" },
              "body": { "type": "string" },
              "body_b64": { "type": "string", "description": "正文字节保真通道（base64；非 UTF-8 正文必须用它，红线27）" },
              "principal": { "type": "string" },
              "session": { "type": "string" } } } },
        { "name": "mem_update", "description": "读改写（全摘要令牌 CAS；冲突返回当前令牌供重读重放）。payload 必填（缺省=清空载荷，M4）",
          "inputSchema": { "type": "object", "required": ["id", "expect", "payload"], "properties": {
              "id": { "type": "string" }, "expect": expect_schema(),
              "payload": { "type": "object" }, "body": { "type": "string" },
              "body_b64": { "type": "string", "description": "正文字节保真通道（base64；非 UTF-8 正文必须用它，红线27）" },
              "session": { "type": "string" } } } },
        { "name": "mem_link", "description": "建立引用（源记录加边；两侧令牌 CAS；派生类 DAG）",
          "inputSchema": { "type": "object", "required": ["a", "b", "rel", "expect_a", "expect_b"], "properties": {
              "a": { "type": "string" }, "b": { "type": "string" }, "rel": { "type": "string" },
              "expect_a": expect_schema(), "expect_b": expect_schema() } } },
        { "name": "mem_delete", "description": "删除（受管 tombstone；引用默认阻止，force 越过并警示）",
          "inputSchema": { "type": "object", "required": ["id", "expect"], "properties": {
              "id": { "type": "string" }, "expect": expect_schema(), "force": { "type": "boolean" } } } },
        { "name": "mem_restore", "description": "恢复 tombstone（attic → records 同路径）",
          "inputSchema": { "type": "object", "required": ["id"], "properties": { "id": { "type": "string" } } } },
        { "name": "mem_restore_body", "description": "恢复正文（经确认的内容写：正文区替换为来源字节，信封/载荷原样保留，版本+1，事件 op=restore_body；R11/BD4）。与 fix 的分界：结构头坏 → 先 fix；正文坏 → mem_restore_body。来源三形：git:<版本> 指定历史版本 | file:<店外路径> 店外备份 | history 自动锚定（仅在可判定窗口内：漂移仍在 ∧ 最后事件为机制写入登记；窗口外响亮拒绝并附指引，绝不回退次优匹配）",
          "inputSchema": { "type": "object", "required": ["id", "expect", "source"], "properties": {
              "id": { "type": "string" }, "expect": expect_schema(),
              "source": { "type": "string", "description": "git:<版本> | file:<店外路径> | history" },
              "session": { "type": "string" } } } },
        { "name": "mem_diff", "description": "增量 = changes_since_N（日志过滤）∪ current_drift（独立预算；与 git 无关）。changes_since 组级 offset 续读（取自上页 resume.offset）",
          "inputSchema": { "type": "object", "properties": { "since": { "type": "integer" },
              "offset": { "type": "integer", "description": "组序号续读（取自上页 resume.offset）" } } } },
        { "name": "mem_reconcile", "description": "写例程：状态覆盖回填 + commit-pending 重试 + 检查点刷新",
          "inputSchema": { "type": "object", "properties": { "deep": { "type": "boolean" } } } },
        { "name": "mem_verify", "description": "全库校验（坏条目、断链、历史空洞、--deep 摘要）",
          "inputSchema": { "type": "object", "properties": { "deep": { "type": "boolean" } } } },
    ])
}
