//! 结果载荷整形（行数夹紧 + 单格字符夹紧 + 长值续读）。
//!
//! 只做载荷整形，不判断 token 预算：行数与单格字符超限属于"限制"语义，
//! 一律夹紧并回显；token 预算属于"预算"语义，必须显式失败或显式摘要，两者不可混用。

use serde_json::Value;

/// 行数上限取值范围。
const MAX_ROWS_CEILING: usize = 1000;
const MAX_ROWS_FLOOR: usize = 1;
/// 单格字符上限取值范围。
const CELL_CHAR_LIMIT_CEILING: usize = 4000;
const CELL_CHAR_LIMIT_FLOOR: usize = 1;
/// 长值续读起点取值范围。
const CELL_CHAR_OFFSET_CEILING: usize = 1_000_000;

/// 整形后的结果集。
#[derive(Debug, Clone, PartialEq)]
pub struct ShapedResult {
    /// 裁剪后的行；每行内被裁剪的字符串单元为截取后的片段。
    pub rows: Vec<Vec<Value>>,
    /// 返回的行数。
    pub row_count: usize,
    /// 行数是否因 max_rows 被截断。
    pub truncated: bool,
    /// 实际生效的 max_rows（夹紧后的值）。
    pub effective_max_rows: usize,
    /// 被截断的字符串单元续读位置，元素为 (行号, 列号, 下一段起始偏移)。
    pub next_offsets: Vec<(usize, usize, usize)>,
}

/// 按行数与单格字符上限整形结果集。
///
/// 行数与单格字符超限一律夹紧并回显，不报错。
/// 行号与列号均以整形后的结果为基准。
///
/// # 参数
///
/// - `rows`：原始行集合。
/// - `max_rows`：返回行数上限，超限部分被丢弃并置 `truncated`。
/// - `cell_char_limit`：单个字符串单元返回的字符上限。
/// - `cell_char_offset`：长值续读起点，按该偏移截取字符串。
pub fn shape_rows(
    rows: &[Vec<Value>],
    max_rows: usize,
    cell_char_limit: usize,
    cell_char_offset: usize,
) -> ShapedResult {
    let effective_max_rows = max_rows.clamp(MAX_ROWS_FLOOR, MAX_ROWS_CEILING);
    let effective_cell_limit =
        cell_char_limit.clamp(CELL_CHAR_LIMIT_FLOOR, CELL_CHAR_LIMIT_CEILING);
    let effective_offset = cell_char_offset.min(CELL_CHAR_OFFSET_CEILING);

    let truncated = rows.len() > effective_max_rows;
    let kept = &rows[..rows.len().min(effective_max_rows)];

    let mut shaped_rows: Vec<Vec<Value>> = Vec::with_capacity(kept.len());
    let mut next_offsets: Vec<(usize, usize, usize)> = Vec::new();

    for (row_index, row) in kept.iter().enumerate() {
        let mut shaped_row: Vec<Value> = Vec::with_capacity(row.len());
        for (column_index, cell) in row.iter().enumerate() {
            match cell {
                Value::String(text) => {
                    let (chunk, next_offset) =
                        clip_cell(text, effective_offset, effective_cell_limit);
                    if let Some(offset) = next_offset {
                        next_offsets.push((row_index, column_index, offset));
                    }
                    shaped_row.push(Value::String(chunk));
                }
                other => shaped_row.push(other.clone()),
            }
        }
        shaped_rows.push(shaped_row);
    }

    ShapedResult {
        row_count: shaped_rows.len(),
        truncated,
        effective_max_rows,
        rows: shaped_rows,
        next_offsets,
    }
}

/// 截取字符串单元；返回 (片段, 续读偏移)。
///
/// 续读偏移为 `None` 表示该单元已完整返回，无需续读。
/// 字符按 Unicode 码点计，与数据源返回的字符语义一致。
fn clip_cell(cell: &str, offset: usize, limit: usize) -> (String, Option<usize>) {
    let total = cell.chars().count();
    if total <= offset {
        return (String::new(), None);
    }
    let start = cell
        .char_indices()
        .nth(offset)
        .map_or(cell.len(), |(index, _)| index);
    let end = cell
        .char_indices()
        .nth(offset.saturating_add(limit))
        .map_or(cell.len(), |(index, _)| index);
    let chunk = &cell[start..end];
    let chunk_chars = chunk.chars().count();
    if total > offset.saturating_add(limit) {
        (chunk.to_string(), Some(offset + chunk_chars))
    } else {
        (chunk.to_string(), None)
    }
}
