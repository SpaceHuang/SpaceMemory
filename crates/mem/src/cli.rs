//! CLI 命令面（设计文档 §3.3 草案）：
//!
//! `brief` / `session start|flush` / `reconcile` / `list` / `find` / `show` / `trace` /
//! `add` / `update --expect` / `link` / `merge` / `diff --since` / `view` / `bind` /
//! `init` / `agenda` / `apply` / `fix` / `verify` / `serve` / `mcp`
//!
//! 命令命名与 MCP 工具面定稿见开放问题29。

use clap::Parser;

use crate::commands;

/// mem —— Agent 记忆与状态层（第二记忆）机制层 CLI（§3.3）。
#[derive(Parser)]
#[command(name = "mem", version, about = "SpaceMemory 机制层：记录/台账/事件/维护")]
pub struct Cli {
    /// 数据目录（默认 $MEM_STORE 或 ~/.agent-memory）。
    #[arg(long, global = true)]
    pub store: Option<std::path::PathBuf>,

    #[command(subcommand)]
    pub cmd: Cmd,
}

#[derive(clap::Subcommand)]
pub enum Cmd {
    /// 建店 + 播种类型两件套 + 自举首条 principal（D9）
    Init {
        path: std::path::PathBuf,
        /// 外部播种资产目录（开放问题40：exe 旁 builtin-types/ 优先，其次本键）
        #[arg(long)]
        template_dir: Option<String>,
        /// 首条 principal 的标签（自举逃逸）
        #[arg(long, default_value = "main")]
        principal: String,
    },
    /// 冷启动工作视图。零写盘；无台账如实报告；索引不可证明 → 直读标注（R1.3/R3.5）
    Brief {
        view: Option<String>,
        #[arg(long)]
        principal: Option<String>,
        #[arg(long)]
        since: Option<u64>,
        #[arg(long, help = "条目续读偏移（配合 resume.offset）")]
        offset: Option<usize>,
    },
    Session {
        #[command(subcommand)]
        sub: SessionCmd,
    },
    /// 写例程：状态覆盖回填 + commit-pending 重试 + 检查点刷新（D5/D12）
    Reconcile {
        #[arg(long)]
        deep: bool,
    },
    /// 过滤定位（LIMIT+续读；返回完整令牌；--filter 载荷字段过滤可重复合取，R10）
    List {
        #[arg(long)]
        view: Option<String>,
        #[arg(long = "type")]
        type_filter: Option<String>,
        /// 载荷字段过滤（字段=值，以第一个 = 分界；可重复、合取）
        #[arg(long = "filter")]
        filters: Vec<String>,
        #[arg(long)]
        limit: Option<usize>,
        #[arg(long)]
        offset: Option<usize>,
    },
    /// 查找（词项与/与位排除、载荷字段过滤；id 前缀/别名/逐值/正文四域子串命中）
    Find {
        query: String,
        /// 载荷字段过滤（字段=值，以第一个 = 分界；可重复、合取）
        #[arg(long = "filter")]
        filters: Vec<String>,
        #[arg(long)]
        limit: Option<usize>,
        #[arg(long, help = "续读偏移（配合上次输出的 resume.offset）")]
        offset: Option<usize>,
    },
    /// 钻取单条（返回令牌；零写盘；正文受 brief_bytes 预算，截断附续读）
    Show {
        id: String,
        #[arg(long, default_value_t = true)]
        body: bool,
        #[arg(long, help = "正文字节续读偏移（配合 resume.body_offset）")]
        body_offset: Option<usize>,
        /// R11.2/BD1：正文原始字节全量落至调用方指定文件（字节取回通道）；
        /// stdout 只回元信息（路径/字节数/正文 SHA-256）。目标已存在或位于
        /// 店目录内 → 响亮拒绝（读命令不做覆盖写、不留旁挂文件）。
        #[arg(long = "body-out")]
        body_out: Option<String>,
    },
    /// 多跳遍历（预算+截断标注）
    Trace {
        id: String,
        #[arg(long, default_value = "up")]
        dir: String,
        #[arg(long)]
        depth: Option<usize>,
        #[arg(long)]
        limit: Option<usize>,
        #[arg(long, help = "跳过前 N 个已见节点（配合 resume.skip 续读）")]
        skip: Option<usize>,
    },
    /// 创建（唯一键路径独占；通道盖章；兜底建台账；追加事件）
    Add {
        #[arg(name = "type")]
        type_name: String,
        #[arg(long, help = "payload JSON（类型声明解释）")]
        payload: String,
        #[arg(long, help = "正文文件路径；'-' = stdin")]
        body: Option<String>,
        #[arg(long)]
        principal: Option<String>,
        #[arg(long)]
        session: Option<String>,
    },
    /// 读改写（全摘要令牌 CAS）
    Update {
        id: String,
        #[arg(long)]
        expect: String,
        #[arg(long)]
        payload: String,
        #[arg(long)]
        body_file: Option<String>,
        #[arg(long)]
        session: Option<String>,
    },
    /// 建立引用（源记录加边；两侧令牌 CAS）
    Link {
        a: String,
        b: String,
        #[arg(long)]
        rel: String,
        #[arg(long)]
        expect_a: String,
        #[arg(long)]
        expect_b: String,
    },
    /// 合并（独占门闩内；别名移交 + 引用重挂 + tombstone 最后）
    Merge {
        dup: String,
        canonical: String,
        #[arg(long)]
        expect_dup: String,
        #[arg(long)]
        expect_canonical: String,
    },
    /// 删除（受管 tombstone；引用默认阻止；force 越过并警示）
    Delete {
        id: String,
        #[arg(long)]
        expect: String,
        #[arg(long, default_value_t = false)]
        force: bool,
    },
    /// 恢复（attic → records 同路径）
    Restore { id: String },
    /// 增量 = changes_since_N（日志过滤）∪ current_drift（独立预算；与 git 无关）
    Diff {
        #[arg(long)]
        since: Option<u64>,
        #[arg(long, help = "changes_since 组级续读偏移（配合 resume.offset）")]
        offset: Option<usize>,
    },
    /// 执行视图（声明式查询模板）
    View {
        id: String,
        #[arg(long)]
        limit: Option<usize>,
        #[arg(long)]
        offset: Option<usize>,
    },
    /// 绑定缺省视图指针（显式声明；禁止环境推断，红线11）
    Bind {
        view: String,
    },
    /// 全库校验（坏条目、断链、历史空洞、半成品计划、--deep 摘要）
    Verify {
        #[arg(long)]
        deep: bool,
    },
    /// 提案队列：扫描（重复/断引用/环/未消化证据/存疑）+ 列出计划
    Agenda {
        /// 同时把扫描到的提案落盘（agenda/<id>.json）
        #[arg(long, default_value_t = false)]
        scan: bool,
    },
    /// 执行计划（独占门闩内逐记录令牌 CAS；executing 检出 → 显式续跑）
    Apply { plan_id: String },
    /// 修复记录（frontmatter 损坏 → git 历史上一版 + 正文按字节保持；或显式提供原料）
    Fix {
        id: String,
        #[arg(long, help = "重建用 envelope JSON（无 git 历史时的原料）")]
        envelope: Option<String>,
        #[arg(long, help = "重建用 payload JSON")]
        payload: Option<String>,
        #[arg(long, help = "修订令牌（记录可解析时必带，红线19；损坏记录豁免）")]
        expect: Option<String>,
    },
    /// 恢复正文（经确认的内容写：正文区替换为来源字节，信封/载荷原样保留；BD4）。
    /// 与 fix 的分界：结构头坏 → 先 fix（保正文字节）；正文坏 → restore-body（换正文字节）
    RestoreBody {
        id: String,
        #[arg(long, help = "修订令牌（全摘要 CAS，红线19）")]
        expect: String,
        /// R11/BD4 来源三形：`git:<版本>` 指定历史版本 | `file:<店外路径>` 店外备份
        /// | `history` 自动锚定（仅在可判定窗口内：漂移仍在 ∧ 最后事件为机制写入；
        /// 窗口外响亮拒绝并附指引，绝不回退次优匹配）
        #[arg(long)]
        source: String,
    },
}

#[derive(clap::Subcommand)]
pub enum SessionCmd {
    /// 显式建立/接手（幂等键 principal+view+task_ref；D13.5）
    Start {
        view: Option<String>,
        #[arg(long)]
        principal: String,
        #[arg(long)]
        task_ref: Option<String>,
        #[arg(long)]
        handoff: Option<String>,
    },
    /// 提交读留痕，置 flushed（可接续）
    Flush {
        #[arg(long)]
        session: String,
        #[arg(long)]
        trail: Vec<String>,
    },
}

pub fn run() {
    let cli = Cli::parse();
    match commands::dispatch(&cli) {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            let json = serde_json::json!({ "error": e.to_string() });
            eprintln!("{json}");
            std::process::exit(1);
        }
    }
}
