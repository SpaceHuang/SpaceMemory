# 公开规范（D8：护城河 = 三个规格）

数据层与机制层之间、类型声明与校验器之间的契约。**三件套已定稿 v1**（2026-11-05，§8 第 1–3 步）：

1. [envelope-format.md](envelope-format.md) —— 信封与目录格式规范 v1（D18：单文件 Markdown + JSON frontmatter；envelope/payload 所有权分块；机制确定性再生成、正文按字节保持；events.jsonl 行格式；锁与提交纪律；mem.json 配置——含 `init.template_dir` 定名、默认预算）
2. [type-declaration.md](type-declaration.md) —— 类型声明两件套格式规范（D19：JSON Schema 2020-12 + x-* 词汇表 + 键字符集规则）+ task 试金石契约
3. [conventions-source.md](conventions-source.md) —— 约定文档单一事实源结构（含角色段、多级下发，R7.4）

规范定稿后进入评审；实现与规范冲突时，以规范评审结论为准。
