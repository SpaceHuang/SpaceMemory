# edge 约定

## 载荷字段语义
- 信封引用（refs）足够时**不要**建 edge——edge 只为"边本身有属性"的场景存在（D16）。
- `from`/`to` 是记录 ID；边记录的 envelope.refs 同步携带 from/to（便于反向索引与级联）。

## 生命周期
- 边记录可删可建；删除边不影响两端记录。

## 提炼与引用
- 派生关系仍优先走 envelope.refs + x-dag 类型；edge 用于带权/带证据的显式关系（如 similarity、support）。
