// T-047：CSV 导入验证与批次导入
//
// 编码检测、列映射、类型预检、首批预览。
// 绑定参数 + 有界批次 + 整单事务（超限分批提交并声明不可整体回滚）。
// 验收：A21 导入中断后已提交批次可查，未提交批次无痕迹。

use serde::{Deserialize, Serialize};

/// 一次导入最多写多少行（默认）
pub const DEFAULT_IMPORT_LIMIT: usize = 1000;
/// 预览给界面看多少行样本（默认）
pub const DEFAULT_SAMPLE_LIMIT: usize = 20;

/// 审批摘要绑定的规范串：**由后端生成**，前端原样拿去算摘要，不自己拼。
///
/// 为什么绑「表 + 行数」而不是 SQL 全文：导入的 INSERT 是**按文件内容动态拼**出来的，
/// 一条审批 grant 绑不住 N 条 INSERT（审批摘要是一对一，见 security::evaluate）。
/// 但"往哪张表写多少行"正是用户在审批弹窗里真正要确认的那两件事，
/// 绑定之后任何一处对不上（换了表、文件在预览之后被改过）都会被摘要挡下来。
///
/// ⚠ 格式两端必须**逐字节一致**（前端 `digestSql` ↔ 后端 `digest_sql`）：
/// 改这里必须同步改前端 `src/ipc/db.ts`，并由两端的黄金向量
/// （`src-tauri/src/security.rs` 与 `tests/digestContract.test.ts`）一起钉住。
pub fn import_canonical(table: &str, rows: u64) -> String {
    format!("CSV_IMPORT table={} rows={}", table, rows)
}

/// 导入结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportResult {
    pub success: bool,
    pub rows_imported: u64,
    pub rows_failed: u64,
    pub errors: Vec<String>,
}

/// 导入计划：界面上确认导入前要看到的全部事实。
///
/// 这不是"预览"那么简单 —— 它同时给出**审批摘要要绑的那份规范串**，
/// 所以前端不需要（也不应该）自己拼摘要输入。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportPlan {
    /// 目标表名（用户可改；改完要重新出计划，摘要才跟着变）
    pub table_name: String,
    pub headers: Vec<String>,
    /// 前 N 行样本
    pub sample_rows: Vec<Vec<String>>,
    /// 文件里的数据行数（原始行数，不含表头；空行也计入）
    pub total_rows: u64,
    /// **真正会被插入的行数**（受 import_limit 约束；空行不计）
    ///
    /// 为什么和 total_rows 分开：文件里可能有空行，而空行不生成 INSERT
    /// ⇒ 用户在审批弹窗里看到的"写多少行"必须是这个数，不是原始行数。
    pub import_rows: u64,
    pub import_limit: usize,
    pub encoding: String,
    /// 编码判定的置信度（0.0-1.0）。
    ///
    /// 为什么要给用户看：置信度低（0.5）意味着"其实是 UTF-8，但不敢保证"。
    /// 只给一个 encoding 名字，用户看到"utf-8"就以为一定是 UTF-8，
    /// 结果导入后中文全是乱码还不知道原因。
    pub encoding_confidence: f64,
    /// 审批摘要绑定的规范串（后端算好，前端原样用）
    pub canonical: String,
}

/// 编码检测结果
#[derive(Debug, Clone)]
pub struct EncodingDetection {
    pub encoding: &'static str,
    pub confidence: f64,
}

/// 检测字节序列的编码类型
pub fn detect_encoding(bytes: &[u8]) -> EncodingDetection {
    // UTF-8 BOM 检查
    if bytes.len() >= 3 && bytes[0] == 0xEF && bytes[1] == 0xBB && bytes[2] == 0xBF {
        return EncodingDetection {
            encoding: "utf-8",
            confidence: 1.0,
        };
    }

    // UTF-8 验证
    if std::str::from_utf8(bytes).is_ok() {
        return EncodingDetection {
            encoding: "utf-8",
            confidence: 0.99,
        };
    }

    // GBK 检查（简体中文常见编码）
    if is_likely_gbk(bytes) {
        return EncodingDetection {
            encoding: "gbk",
            confidence: 0.9,
        };
    }

    EncodingDetection {
        encoding: "utf-8",
        confidence: 0.5,
    }
}

/// 简化的 GBK 检测（检查双字节模式）
fn is_likely_gbk(bytes: &[u8]) -> bool {
    let mut i = 0;
    let mut gbk_chars = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b < 0x80 {
            i += 1;
        } else if (0xB0..=0xF7).contains(&b) {
            // GBK 首字节范围
            if i + 1 < bytes.len() {
                let next = bytes[i + 1];
                if (0x40..=0xFE).contains(&next) {
                    gbk_chars += 1;
                    i += 2;
                    continue;
                }
            }
            i += 1;
        } else {
            i += 1;
        }
    }
    gbk_chars > 10
}

/// 把字节解码成文本 + 编码判定。
///
/// 抽出来的理由：**行数与样本行必须来自同一份解码结果**。
/// 之前 `preview_csv` 拿原始字节去数行（`from_utf8(bytes).unwrap_or("")`），
/// GBK 文件解码失败 ⇒ 数出来 0 行，而样本行却是按 GBK 解出来的 ⇒ 两个数对不上。
pub fn decode_text(bytes: &[u8]) -> Result<(String, EncodingDetection), String> {
    let detection = detect_encoding(bytes);
    let text = match detection.encoding {
        "utf-8" => std::str::from_utf8(bytes)
            .map_err(|e| format!("UTF-8 解码失败: {}", e))?
            .to_string(),
        "gbk" => {
            // GBK -> UTF-8 简单转换（实际应用应使用 encoding_rs）
            let mut result = String::new();
            let mut i = 0;
            while i < bytes.len() {
                let b = bytes[i];
                if b < 0x80 {
                    result.push(b as char);
                    i += 1;
                } else if (0xB0..=0xF7).contains(&b) && i + 1 < bytes.len() {
                    result.push('\u{FFFD}'); // 替换字符
                    i += 2;
                } else {
                    result.push('\u{FFFD}');
                    i += 1;
                }
            }
            result
        }
        _ => std::str::from_utf8(bytes)
            .map_err(|e| format!("解码失败: {}", e))?
            .to_string(),
    };
    Ok((text, detection))
}

/// 解析 CSV 内容为结构化数据
pub fn parse_csv(
    bytes: &[u8],
    delimiter: char,
    limit: usize,
) -> Result<(Vec<String>, Vec<Vec<String>>), String> {
    let (text, _) = decode_text(bytes)?;
    Ok(parse_csv_text(&text, delimiter, limit))
}

/// 从**已解码**文本解析 CSV
pub fn parse_csv_text(
    text: &str,
    delimiter: char,
    limit: usize,
) -> (Vec<String>, Vec<Vec<String>>) {
    let mut reader = text.lines();
    let mut headers = Vec::new();
    let mut rows = Vec::new();
    let mut row_count = 0;

    if let Some(line) = reader.next() {
        headers = parse_csv_line(line, delimiter);
    }

    for line in reader {
        if row_count >= limit {
            break;
        }
        let row = parse_csv_line(line, delimiter);
        if !row.is_empty() && !row.iter().all(|s| s.is_empty()) {
            rows.push(row);
            row_count += 1;
        }
    }

    (headers, rows)
}

/// 解析单行 CSV（支持引号字段）
fn parse_csv_line(line: &str, delimiter: char) -> Vec<String> {
    let mut fields = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut chars = line.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            '"' => {
                if in_quotes {
                    // 检查转义引号 ""
                    if chars.peek() == Some(&'"') {
                        current.push('"');
                        chars.next();
                    } else {
                        in_quotes = false;
                    }
                } else {
                    in_quotes = true;
                }
            }
            c if c == delimiter && !in_quotes => {
                fields.push(current.clone());
                current.clear();
            }
            '\r' => {
                // 忽略 CR
            }
            c => {
                current.push(c);
            }
        }
    }

    if !current.is_empty() || in_quotes {
        fields.push(current);
    }

    fields
}

/// 出一份导入计划：样本行 + 真正会写多少行 + 审批摘要要绑的规范串。
///
/// ⚠ `import_rows` 必须与 `csv_import_execute` 里算出来的行数**完全一致**，
/// 否则用户批准的摘要对不上实际要写的行数 ⇒ 导入被拒（这是有意的：宁可拒，
/// 不能"批准的是 10 行、实际写了 12 行"）。
pub fn plan_import(
    bytes: &[u8],
    delimiter: char,
    sample_limit: usize,
    import_limit: usize,
    table_name: &str,
) -> Result<ImportPlan, String> {
    let (text, detection) = decode_text(bytes)?;
    // 一次解析拿到上限内的全部行；样本只是它的前 N 行
    let (headers, rows) = parse_csv_text(&text, delimiter, import_limit.max(sample_limit));
    let import_rows = rows.len().min(import_limit) as u64;
    let sample_rows = rows.iter().take(sample_limit).cloned().collect();

    // 数据行数 = 总行数 - 表头行（空行也算数据行）
    let total_lines = text.lines().count() as u64;
    let total_rows = total_lines.saturating_sub(1);

    Ok(ImportPlan {
        table_name: table_name.to_string(),
        headers,
        sample_rows,
        total_rows,
        import_rows,
        import_limit,
        encoding: detection.encoding.to_string(),
        encoding_confidence: detection.confidence,
        canonical: import_canonical(table_name, import_rows),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_utf8() {
        let bytes = "hello,world\n1,2".as_bytes();
        let det = detect_encoding(bytes);
        assert_eq!(det.encoding, "utf-8");
        assert!(det.confidence > 0.9);
    }

    #[test]
    fn detect_utf8_bom() {
        let bytes = [0xEF, 0xBB, 0xBF, b'h', b'i'];
        let det = detect_encoding(&bytes);
        assert_eq!(det.encoding, "utf-8");
        assert_eq!(det.confidence, 1.0);
    }

    #[test]
    fn parse_simple_csv() {
        let csv = b"name,age,city\nAlice,30,Beijing\nBob,25,Shanghai";
        let (headers, rows) = parse_csv(csv, ',', 10).unwrap();
        assert_eq!(headers, vec!["name", "age", "city"]);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], vec!["Alice", "30", "Beijing"]);
    }

    #[test]
    fn parse_quoted_fields() {
        let csv = b"name,desc\nAlice,\"Hello, World\"\nBob,\"Say \"\"hi\"\"\"";
        let (headers, rows) = parse_csv(csv, ',', 10).unwrap();
        assert_eq!(headers, vec!["name", "desc"]);
        assert_eq!(rows[0][1], "Hello, World");
        assert_eq!(rows[1][1], "Say \"hi\"");
    }

    #[test]
    fn plan_import_counts_rows_and_binds_canonical() {
        let csv = b"a,1\nb,2\nc,3\nd,4\ne,5";
        let plan = plan_import(csv, ',', 3, 1000, "t").unwrap();
        assert_eq!(plan.headers, vec!["a", "1"]);
        assert_eq!(plan.sample_rows.len(), 3, "样本行数按 sample_limit 截断");
        assert_eq!(plan.total_rows, 4, "数据行数 = 总行数 - 表头");
        assert_eq!(plan.import_rows, 4, "真正会插入的行数");
        assert_eq!(plan.canonical, "CSV_IMPORT table=t rows=4");
        assert_eq!(plan.table_name, "t");
    }

    #[test]
    fn plan_import_respects_import_limit() {
        let csv = b"a,1\nb,2\nc,3\nd,4\ne,5";
        let plan = plan_import(csv, ',', 10, 2, "t").unwrap();
        assert_eq!(plan.import_rows, 2, "导入上限截断真正写入的行数");
        assert_eq!(plan.total_rows, 4, "原始行数不受导入上限影响");
        assert_eq!(plan.canonical, "CSV_IMPORT table=t rows=2");
    }

    #[test]
    fn plan_import_skips_blank_lines_in_import_rows() {
        // 空行不生成 INSERT ⇒ 审批摘要绑的是"真正会写的行数"，不是原始行数
        let csv = b"h1,h2\nv1,v2\n\nv3,v4\n\n";
        let plan = plan_import(csv, ',', 10, 1000, "t").unwrap();
        assert_eq!(plan.total_rows, 4, "原始数据行数按行算（含空行）");
        assert_eq!(plan.import_rows, 2, "空行不插入");
        assert_eq!(plan.canonical, "CSV_IMPORT table=t rows=2");
    }

    #[test]
    fn plan_import_canonical_changes_with_table() {
        let csv = b"a,1\nb,2";
        let p1 = plan_import(csv, ',', 10, 1000, "t1").unwrap();
        let p2 = plan_import(csv, ',', 10, 1000, "t2").unwrap();
        assert_ne!(p1.canonical, p2.canonical, "换表 ⇒ 摘要必须变");
    }

    #[test]
    fn empty_csv() {
        let csv = b"";
        let plan = plan_import(csv, ',', 10, 1000, "t").unwrap();
        assert!(plan.headers.is_empty());
        assert!(plan.sample_rows.is_empty());
        assert_eq!(plan.import_rows, 0);
    }

    #[test]
    fn gbk_file_line_count_matches_decoded_text() {
        // GBK 字节（"中文" 的 GBK 编码）：解码失败时旧实现会数出 0 行
        let mut gbk: Vec<u8> = Vec::new();
        for _ in 0..6 {
            gbk.extend_from_slice(&[0xD6, 0xD0, 0xCE, 0xC4]); // "中文"
        }
        gbk.push(b'\n');
        gbk.extend_from_slice(b"1,2");
        let plan = plan_import(&gbk, ',', 10, 1000, "t").unwrap();
        assert_eq!(plan.encoding, "gbk");
        assert_eq!(plan.total_rows, 1, "行数必须来自解码后的文本，不是原始字节");
        assert_eq!(plan.import_rows, 1);
    }
}
