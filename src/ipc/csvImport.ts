// CSV 导入（预览 → 审批 → 写入）与后端 src-tauri/src/db/csv_import.rs 配对。
//
// 这个模块存在的理由不是"放几个类型"，而是**把审批摘要要绑的那份文本的来源钉死**：
// 导入的 INSERT 是按文件内容动态拼出来的，前端自己拼摘要输入必然和后端对不上
// ⇒ canonical 由后端（csv_import_plan）算出来，前端**原样**拿去算 digest。
//
// 换句话说：前端**没有** buildCsvImportCanonical 这个函数，这是故意的。

/** 一次导入最多写多少行（与后端 DEFAULT_IMPORT_LIMIT 一致） */
export const CSV_IMPORT_LIMIT = 1000;

/** 预览给界面看多少行样本（与后端 DEFAULT_SAMPLE_LIMIT 一致） */
export const CSV_SAMPLE_LIMIT = 20;

/** 后端 csv_import_plan 的返回 */
export interface CsvImportPlan {
  /** 目标表名 */
  table_name: string;
  /** 列头 */
  headers: string[];
  /** 前 N 行样本 */
  sample_rows: string[][];
  /** 文件里的数据行数（原始行数，不含表头；空行也计入） */
  total_rows: number;
  /** 真正会被插入的行数（受 import_limit 约束；空行不计） */
  import_rows: number;
  import_limit: number;
  encoding: string;
  /** 编码判定置信度（0-1）；低说明"其实是 UTF-8，但不敢保证" */
  encoding_confidence: number;
  /** 审批摘要绑定的规范串 —— 后端算好，前端原样用 */
  canonical: string;
}

/** 后端 csv_import_execute 的返回 */
export interface CsvImportResult {
  success: boolean;
  rows_imported: number;
  rows_failed: number;
  errors: string[];
}

/**
 * 从文件名推一个默认表名。
 *
 * 只是**默认**：用户可以在预览弹窗里改，改完必须重新出计划（canonical 才跟着变）。
 */
export function defaultTableName(filePath: string): string {
  const base = filePath.split(/[\\/]/).pop() ?? "";
  const stem = base.replace(/\.[^.]+$/, "");
  return stem || "imported";
}

/**
 * 预览里的「这次到底会写什么」一句话。
 *
 * 为什么把行数说清楚：文件里可能有空行（空行不生成 INSERT），
 * 用户看到「共 5 行」以为要写 5 行，实际只写 4 行 ⇒ 审批时点头点的是另一件事。
 */
export function describeImport(plan: CsvImportPlan): string {
  const enc =
    plan.encoding_confidence >= 0.9
      ? plan.encoding
      : `${plan.encoding}（置信度 ${(plan.encoding_confidence * 100).toFixed(0)}%，可能乱码）`;
  const truncated = plan.total_rows > plan.import_rows ? `，文件共 ${plan.total_rows} 行` : "";
  return `将向表 ${plan.table_name} 写入 ${plan.import_rows} 行${truncated}；编码 ${enc}`;
}
