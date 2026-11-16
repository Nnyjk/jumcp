//! 载荷整形契约（限制语义：夹紧 + 回显，不报错）。
//!
//! 行数与单格字符超限一律夹紧并回显 effective_max_rows / next_offset；
//! 本模块不判断 token 预算，token 超限属于预算语义，由上层显式失败或摘要。

use jumcp_sqlguard::{ShapedResult, shape_rows};
use serde_json::{Value, json};

fn rows(values: &[Value]) -> Vec<Vec<Value>> {
    values.iter().map(|value| vec![value.clone()]).collect()
}

fn cell(result: &ShapedResult, row: usize, column: usize) -> &Value {
    &result.rows[row][column]
}

#[test]
fn rows_truncated_to_max_rows() {
    let input = rows(&[json!(0), json!(1), json!(2), json!(3), json!(4)]);
    let result = shape_rows(&input, 3, 200, 0);
    assert_eq!(result.rows, rows(&[json!(0), json!(1), json!(2)]));
    assert_eq!(result.row_count, 3);
    assert!(result.truncated);
    assert_eq!(result.effective_max_rows, 3);
}

#[test]
fn rows_not_truncated_when_within_limit() {
    let input = rows(&[json!(1), json!(2)]);
    let result = shape_rows(&input, 3, 200, 0);
    assert_eq!(result.rows, rows(&[json!(1), json!(2)]));
    assert_eq!(result.row_count, 2);
    assert!(!result.truncated);
}

macro_rules! max_rows_clamped_case {
    ($name:ident, $requested:expr, $effective:expr) => {
        #[test]
        fn $name() {
            let input = rows(&[json!(1)]);
            let result = shape_rows(&input, $requested, 200, 0);
            assert_eq!(result.effective_max_rows, $effective);
        }
    };
}

max_rows_clamped_case!(max_rows_zero_floors_to_one, 0, 1);
max_rows_clamped_case!(max_rows_one_kept, 1, 1);
max_rows_clamped_case!(max_rows_upper_bound_kept, 1000, 1000);
max_rows_clamped_case!(max_rows_above_upper_clamped, 5000, 1000);
max_rows_clamped_case!(max_rows_far_above_upper_clamped, 1_000_000_000, 1000);

#[test]
fn max_rows_zero_still_returns_one_row() {
    let input = rows(&[json!(1), json!(2)]);
    let result = shape_rows(&input, 0, 200, 0);
    assert_eq!(result.rows, rows(&[json!(1)]));
    assert_eq!(result.row_count, 1);
    assert!(result.truncated);
}

// ---------- 单格字符夹紧与续读 ----------

#[test]
fn long_string_cell_truncated_with_next_offset() {
    let input = rows(&[json!("abcdefghij")]);
    let result = shape_rows(&input, 100, 4, 0);
    assert_eq!(result.rows, rows(&[json!("abcd")]));
    assert_eq!(result.next_offsets, vec![(0, 0, 4)]);
}

#[test]
fn cell_exactly_at_limit_not_truncated() {
    let input = rows(&[json!("abcd")]);
    let result = shape_rows(&input, 100, 4, 0);
    assert_eq!(result.rows, rows(&[json!("abcd")]));
    assert!(result.next_offsets.is_empty());
}

#[test]
fn next_offset_continues_from_offset() {
    let input = rows(&[json!("abcdefghij")]);
    let result = shape_rows(&input, 100, 3, 4);
    assert_eq!(result.rows, rows(&[json!("efg")]));
    assert_eq!(result.next_offsets, vec![(0, 0, 7)]);
}

#[test]
fn offset_beyond_cell_length_returns_empty_and_no_offset() {
    let input = rows(&[json!("abc")]);
    let result = shape_rows(&input, 100, 10, 10);
    assert_eq!(result.rows, rows(&[json!("")]));
    assert!(result.next_offsets.is_empty());
}

macro_rules! cell_char_limit_clamped_case {
    ($name:ident, $requested:expr, $effective:expr) => {
        #[test]
        fn $name() {
            let input = rows(&[json!("xxxxxxxxxx")]);
            let result = shape_rows(&input, 100, $requested, 0);
            let length = cell(&result, 0, 0).as_str().unwrap().chars().count();
            assert_eq!(length, $effective.min(10));
        }
    };
}

cell_char_limit_clamped_case!(cell_char_limit_zero_floors_to_one, 0, 1);
cell_char_limit_clamped_case!(cell_char_limit_one_kept, 1, 1);
cell_char_limit_clamped_case!(cell_char_limit_upper_bound_kept, 4000, 4000);
cell_char_limit_clamped_case!(cell_char_limit_above_upper_clamped, 9999, 4000);

macro_rules! cell_char_offset_clamped_case {
    ($name:ident, $requested:expr, $effective:expr) => {
        #[test]
        fn $name() {
            let input = rows(&[json!("abcdefghij")]);
            let result = shape_rows(&input, 100, 2, $requested);
            let expected_start = $effective.min(10);
            let expected: String = "abcdefghij".chars().skip(expected_start).take(2).collect();
            assert_eq!(cell(&result, 0, 0).as_str(), Some(expected.as_str()));
        }
    };
}

cell_char_offset_clamped_case!(cell_char_offset_zero_kept, 0, 0);
cell_char_offset_clamped_case!(cell_char_offset_upper_bound_kept, 1_000_000, 1_000_000);
cell_char_offset_clamped_case!(cell_char_offset_above_upper_clamped, 2_000_000, 1_000_000);

// ---------- 非字符串单元不产生 offset ----------

#[test]
fn non_string_cells_produce_no_offsets() {
    let input = vec![vec![
        Value::Null,
        json!(123),
        json!(4.5),
        json!(["bytes"]),
        json!(true),
    ]];
    let result = shape_rows(&input, 100, 1, 0);
    assert_eq!(result.rows, input);
    assert!(result.next_offsets.is_empty());
}

#[test]
fn mixed_row_only_string_cell_reports_offset() {
    let input = vec![vec![Value::Null, json!("abcdef"), json!(42)]];
    let result = shape_rows(&input, 100, 2, 0);
    assert_eq!(result.rows, vec![vec![Value::Null, json!("ab"), json!(42)]]);
    assert_eq!(result.next_offsets, vec![(0, 1, 2)]);
}

#[test]
fn offsets_are_per_cell_coordinates() {
    let input = vec![
        vec![json!("aaaa"), json!("bb")],
        vec![json!("cc"), json!("dddd")],
    ];
    let result = shape_rows(&input, 100, 2, 0);
    assert_eq!(result.next_offsets, vec![(0, 0, 2), (1, 1, 2)]);
}

#[test]
fn offsets_only_for_kept_rows() {
    let input = vec![vec![json!("aaaa")], vec![json!("bbbb")]];
    let result = shape_rows(&input, 1, 2, 0);
    assert_eq!(result.next_offsets, vec![(0, 0, 2)]);
    assert_eq!(result.row_count, 1);
}

// ---------- 空输入 ----------

#[test]
fn empty_rows() {
    let result = shape_rows(&[], 100, 200, 0);
    assert!(result.rows.is_empty());
    assert_eq!(result.row_count, 0);
    assert!(!result.truncated);
    assert!(result.next_offsets.is_empty());
}
