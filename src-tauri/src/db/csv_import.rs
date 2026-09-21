// T-047：CSV 导入验证与批次导入
//
// 编码检测、列映射、类型预检、首批预览。
// 绑定参数 + 有界批次 + 整单事务（超限分批提交并声明不可整体回滚）。
// 验收：A21 导入中断后已提交批次可查，未提交批次无痕迹。

use serde::{Deserialize, Serialize};
use std::io::Read;

/// 导入结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportResult {
    pub success: bool,
    pub rows_imported: u64,
    pub rows_failed: u64,
    pub errors: Vec<String>,
    pub preview: Vec<Vec<String>>,
}

/// 预览结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreviewResult {
    pub headers: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub total_rows: u64,
    pub encoding: String,
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
        } else if b >= 0xB0 && b <= 0xF7 {
            // GBK 首字节范围
            if i + 1 < bytes.len() {
                let next = bytes[i + 1];
                if next >= 0x40 && next <= 0xFE {
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

/// 解析 CSV 内容为结构化数据
pub fn parse_csv(bytes: &[u8], delimiter: char, limit: usize) -> Result<(Vec<String>, Vec<Vec<String>>), String> {
    let detection = detect_encoding(bytes);
    
    let text = match detection.encoding {
        "utf-8" => {
            std::str::from_utf8(bytes)
                .map_err(|e| format!("UTF-8 解码失败: {}", e))?
                .to_string()
        }
        "gbk" => {
            // GBK -> UTF-8 简单转换（实际应用应使用 encoding_rs）
            let mut result = String::new();
            let mut i = 0;
            while i < bytes.len() {
                let b = bytes[i];
                if b < 0x80 {
                    result.push(b as char);
                    i += 1;
                } else if b >= 0xB0 && b <= 0xF7 && i + 1 < bytes.len() {
                    result.push('\u{FFFD}'); // 替换字符
                    i += 2;
                } else {
                    result.push('\u{FFFD}');
                    i += 1;
                }
            }
            result
        }
        _ => {
            std::str::from_utf8(bytes)
                .map_err(|e| format!("解码失败: {}", e))?
                .to_string()
        }
    };
    
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
    
    Ok((headers, rows))
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

/// CSV 预览（返回前 N 行）
pub fn preview_csv(
    bytes: &[u8],
    delimiter: char,
    limit: usize,
) -> Result<PreviewResult, String> {
    let detection = detect_encoding(bytes);
    let (headers, rows) = parse_csv(bytes, delimiter, limit)?;
    
    // 转换为字符串计算行数
    let text = std::str::from_utf8(bytes).unwrap_or("");
    let total_lines = text.lines().count().max(1);
    let data_rows = if total_lines > 0 { total_lines - 1 } else { 0 };
    
    Ok(PreviewResult {
        headers,
        rows,
        total_rows: data_rows as u64,
        encoding: detection.encoding.to_string(),
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
    fn preview_with_limit() {
        let csv = b"a,1\nb,2\nc,3\nd,4\ne,5";
        let result = preview_csv(csv, ',', 3).unwrap();
        assert_eq!(result.headers, vec!["a", "1"]);
        assert_eq!(result.rows.len(), 3);
        assert_eq!(result.total_rows, 4);
    }
    
    #[test]
    fn empty_csv() {
        let csv = b"";
        let result = preview_csv(csv, ',', 10).unwrap();
        assert!(result.headers.is_empty());
        assert!(result.rows.is_empty());
    }
}
