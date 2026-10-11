//! Page-range expressions such as `1-3,5,8-end`, `odd`, `even` or `last`.
//!
//! Page numbers are 1-based everywhere a person types them, and every
//! function here returns 1-based numbers in the order the expression gives
//! them, so `5-3` means pages 5, 4, 3.

use anyhow::{Result, bail};

/// Parses a page-range expression against a document with `page_count` pages.
///
/// Accepted pieces, separated by commas:
/// - `7` a single page, `end` or `last` the final page
/// - `2-5` an inclusive range, `5-2` the same range reversed
/// - `3-` or `3-end` from page 3 to the end
/// - `odd`, `even`, `all`
/// - `!4` or `!2-3` removes pages from everything selected so far
pub fn parse_page_ranges(expression: &str, page_count: u32) -> Result<Vec<u32>> {
    if page_count == 0 {
        bail!("the document has no pages");
    }
    let mut selected: Vec<u32> = Vec::new();
    for raw_piece in expression.split(',') {
        let piece = raw_piece.trim().to_ascii_lowercase();
        if piece.is_empty() {
            continue;
        }
        if let Some(excluded) = piece.strip_prefix('!') {
            let removed = parse_piece(excluded, page_count)?;
            selected.retain(|page| !removed.contains(page));
            continue;
        }
        selected.extend(parse_piece(&piece, page_count)?);
    }
    if selected.is_empty() {
        bail!("the range \"{expression}\" selects no pages");
    }
    Ok(selected)
}

fn parse_piece(piece: &str, page_count: u32) -> Result<Vec<u32>> {
    match piece {
        "all" => return Ok((1..=page_count).collect()),
        "odd" => return Ok((1..=page_count).step_by(2).collect()),
        "even" => return Ok((2..=page_count).step_by(2).collect()),
        _ => {}
    }
    if let Some((start_text, end_text)) = piece.split_once('-') {
        let start = parse_page_number(start_text, page_count, 1)?;
        let end = parse_page_number(end_text, page_count, page_count)?;
        return Ok(if start <= end {
            (start..=end).collect()
        } else {
            (end..=start).rev().collect()
        });
    }
    Ok(vec![parse_page_number(piece, page_count, page_count)?])
}

/// Reads one page number; an empty string means `default_when_empty`.
fn parse_page_number(text: &str, page_count: u32, default_when_empty: u32) -> Result<u32> {
    let text = text.trim();
    let page = match text {
        "" => default_when_empty,
        "end" | "last" => page_count,
        number => number
            .parse::<u32>()
            .map_err(|_| anyhow::anyhow!("\"{number}\" is not a page number"))?,
    };
    if page == 0 || page > page_count {
        bail!("page {page} is outside the document (1 to {page_count})");
    }
    Ok(page)
}

/// Splits `1..=page_count` into consecutive chunks of `pages_per_chunk`.
pub fn chunk_pages(page_count: u32, pages_per_chunk: u32) -> Vec<Vec<u32>> {
    let pages_per_chunk = pages_per_chunk.max(1);
    (1..=page_count)
        .collect::<Vec<_>>()
        .chunks(pages_per_chunk as usize)
        .map(|chunk| chunk.to_vec())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_mixed_expressions() {
        assert_eq!(parse_page_ranges("1-3,5,end", 8).unwrap(), vec![1, 2, 3, 5, 8]);
        assert_eq!(parse_page_ranges("odd", 5).unwrap(), vec![1, 3, 5]);
        assert_eq!(parse_page_ranges("6-", 8).unwrap(), vec![6, 7, 8]);
        assert_eq!(parse_page_ranges("4-2", 8).unwrap(), vec![4, 3, 2]);
        assert_eq!(parse_page_ranges("all,!2-3", 4).unwrap(), vec![1, 4]);
    }

    #[test]
    fn rejects_pages_outside_the_document() {
        assert!(parse_page_ranges("9", 8).is_err());
        assert!(parse_page_ranges("0", 8).is_err());
        assert!(parse_page_ranges("x", 8).is_err());
    }

    #[test]
    fn chunks_evenly_with_a_short_tail() {
        assert_eq!(chunk_pages(5, 2), vec![vec![1, 2], vec![3, 4], vec![5]]);
    }
}
