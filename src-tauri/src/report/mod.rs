// 报表语义层引擎
//
// 设计取舍（参考 z-opc-foundation/z-report）：AI 与用户提交的是「声明式规格」
// （dataset / view 的 JSON），由本引擎在内存里求值；而不是把自由文本 SQL 直接
// 打到数据库上。收益：
//   1. 计算列 / 过滤条件不可能触发注入
//   2. 同一条规格可跨 MySQL / PostgreSQL / SQLite 复用，不依赖方言函数
//   3. 跨库联邦：每张表声明自己的 connection_id，按源拉数后内存 hash join
//   4. AI 幻觉在求值前就被本地解析器挡掉（未知列 / 未知函数直接报错）

pub mod expr;
pub mod table;
