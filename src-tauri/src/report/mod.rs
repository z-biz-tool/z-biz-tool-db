// 报表语义层引擎
//
// 设计取舍（参考 z-opc-foundation/z-report）：AI 与用户提交的是「声明式规格」
// （dataset / view 的 JSON），由本引擎在内存里求值；而不是把自由文本 SQL 直接
// 打到数据库上。收益：
//   1. 计算列 / 过滤条件不可能触发注入
//   2. 同一条规格可跨 MySQL / PostgreSQL / SQLite 复用，不依赖方言函数
//   3. 跨库联邦：每张表声明自己的 connection_id，按源拉数后内存 hash join
//   4. AI 幻觉在求值前就被本地解析器挡掉（未知列 / 未知函数直接报错）

pub mod ai;
pub mod dataset;
pub mod expr;
pub mod source;
pub mod table;
pub mod view;

use serde::{de, Deserialize};

/// 枚举字面量按大小写不敏感反序列化。
///
/// 模型会写 "SUM"、"sum"、"Count_Distinct"、"left outer"，每种都值得收下：
/// 一次不匹配就是一次重试往返，而重试是要花钱、要等网络的。
pub(crate) fn de_ci<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: std::str::FromStr<Err = String>,
{
    let raw = String::deserialize(d)?;
    raw.parse::<T>().map_err(de::Error::custom)
}
