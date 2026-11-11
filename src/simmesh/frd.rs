//! CalculiX results (`.frd`, ASCII): nodes, elements and nodal result blocks.

use std::collections::HashMap;

use anyhow::{bail, Context, Result};

use crate::mesh::{Field, Mesh, MAX_FIELDS};

use super::*;

/// Nodes of the FRD element types 1–12 and their VTK cell type.
pub(super) fn frd_element(ty: usize) -> Option<(usize, u8)> {
    Some(match ty {
        1 => (8, HEXAHEDRON),
        2 => (6, WEDGE),
        3 => (4, TETRA),
        4 => (20, QUADRATIC_HEXAHEDRON),
        5 => (15, QUADRATIC_WEDGE),
        6 => (10, QUADRATIC_TETRA),
        7 => (3, TRIANGLE),
        8 => (6, QUADRATIC_TRIANGLE),
        9 => (4, QUAD),
        10 => (8, QUADRATIC_QUAD),
        11 => (2, LINE),
        12 => (3, QUADRATIC_EDGE),
        _ => return None,
    })
}

/// Fixed-width record of an FRD data line: `' -1'`, the id in `width`
/// columns, then numbers in 12 columns (E12.5). Falls back to whitespace
/// separated tokens for files written by other programs.
pub(super) fn frd_record(
    line: &str,
    width: usize,
    values: usize,
    out: &mut Vec<f64>,
) -> Option<i64> {
    let start = out.len();
    let fixed = (|| {
        let id = line.get(3..3 + width)?.trim();
        let id = if id.is_empty() { 0 } else { id.parse().ok()? };
        for k in 0..values {
            let at = 3 + width + 12 * k;
            let s = line.get(at..(at + 12).min(line.len()))?.trim();
            out.push(s.parse().ok()?);
        }
        Some(id)
    })();
    if fixed.is_some() {
        return fixed;
    }
    out.truncate(start);
    let mut tokens = line.split_ascii_whitespace();
    let tag = tokens.next()?;
    let id = if tag == "-1" {
        tokens.next()?.parse().ok()?
    } else {
        0
    };
    for _ in 0..values {
        out.push(tokens.next()?.parse().ok()?);
    }
    Some(id)
}

/// Integers of an FRD element line, `width` columns each from column 3.
pub(super) fn frd_ints(line: &str, width: usize, out: &mut Vec<i64>) -> Option<()> {
    let mut at = 3;
    let mut any = false;
    while at < line.len() {
        let s = line.get(at..(at + width).min(line.len()))?.trim();
        if !s.is_empty() {
            out.push(s.parse().ok()?);
            any = true;
        }
        at += width;
    }
    any.then_some(())
}

/// Priority of FRD result blocks in the field selector (stress first).
pub(super) fn frd_priority(name: &str) -> u8 {
    if name.starts_with("STRESS") {
        0
    } else if name.starts_with("DISP") {
        1
    } else {
        2
    }
}

/// CalculiX result files (`.frd`, ASCII). Nodes, elements and every nodal
/// result block are read; a block that occurs several times (one per
/// increment) shows its last increment.
pub fn frd(data: &[u8]) -> Result<Mesh> {
    let text = String::from_utf8_lossy(data);
    let mut lines = text.lines().map(|l| l.trim_end_matches('\r'));
    let mut grid = Grid::default();
    let mut ids: HashMap<i64, u32> = HashMap::new();
    let mut width = 10;
    // (block name, field) in order of appearance
    let mut fields: Vec<(String, Field)> = Vec::new();
    let mut seen_nodes = false;
    let mut buf: Vec<f64> = Vec::new();
    let mut ints: Vec<i64> = Vec::new();
    while let Some(line) = lines.next() {
        let head = line.trim_start();
        if head.starts_with("2C") || head.starts_with("3C") {
            let tokens: Vec<&str> = head.split_ascii_whitespace().collect();
            let format = if tokens.len() >= 3 {
                tokens.last().and_then(|t| t.parse::<u8>().ok())
            } else {
                None
            };
            match format {
                Some(2) => bail!("Binary FRD files have no preview – write the results as ASCII (*NODE FILE, OUTPUT=ASCII)."),
                Some(0) => width = 5,
                Some(_) => width = 10,
                None => {}
            }
            if head.starts_with("2C") {
                if seen_nodes {
                    bail!("the FRD file has two node blocks");
                }
                seen_nodes = true;
                for line in lines.by_ref() {
                    let tag = line.trim_start();
                    if tag.starts_with("-3") {
                        break;
                    }
                    if !tag.starts_with("-1") {
                        bail!("unexpected line in the FRD node block");
                    }
                    buf.clear();
                    let id =
                        frd_record(line, width, 3, &mut buf).context("invalid FRD node line")?;
                    if ids.len() >= MAX_NODES {
                        bail!("The result has more than {MAX_NODES} nodes – too many for the preview.");
                    }
                    let next = grid.point_count() as u32;
                    match ids.entry(id) {
                        std::collections::hash_map::Entry::Occupied(o) => {
                            // a node written twice: the later coordinates win
                            let at = *o.get() as usize * 3;
                            let point = grid.points.get_mut(at..at + 3).unwrap_or_default();
                            for (p, &c) in point.iter_mut().zip(&buf) {
                                *p = c as f32;
                            }
                        }
                        std::collections::hash_map::Entry::Vacant(v) => {
                            v.insert(next);
                            grid.points.extend(buf.iter().map(|&c| c as f32));
                        }
                    }
                }
            } else {
                // elements: ' -1' id type group material, then ' -2' node lines
                let mut current: Option<(usize, u8)> = None;
                ints.clear();
                let finish = |grid: &mut Grid,
                              current: Option<(usize, u8)>,
                              ints: &mut Vec<i64>|
                 -> Result<()> {
                    if let Some((nodes, ty)) = current {
                        let element = ints
                            .get(..nodes)
                            .context("an FRD element has too few nodes")?;
                        grid.check_cells(1, nodes)?;
                        for id in element {
                            let p = ids
                                .get(id)
                                .context("an FRD element refers to a missing node")?;
                            grid.conn.push(*p);
                        }
                        grid.ends.push(grid.conn.len());
                        grid.types.push(ty);
                    }
                    ints.clear();
                    Ok(())
                };
                for line in lines.by_ref() {
                    let tag = line.trim_start();
                    if tag.starts_with("-3") {
                        break;
                    } else if tag.starts_with("-1") {
                        finish(&mut grid, current.take(), &mut ints)?;
                        // id (width) then type (I5): read the type column directly
                        let ty = line
                            .get(3 + width..3 + width + 5)
                            .and_then(|s| s.trim().parse::<usize>().ok())
                            .or_else(|| {
                                line.split_ascii_whitespace()
                                    .nth(2)
                                    .and_then(|s| s.parse().ok())
                            })
                            .context("invalid FRD element line")?;
                        // unknown types (springs, …) are read and not drawn
                        current = Some(frd_element(ty).unwrap_or((0, 0)));
                    } else if tag.starts_with("-2") {
                        if frd_ints(line, width, &mut ints).is_none() {
                            bail!("invalid FRD element node line");
                        }
                        if ints.len() > 64 {
                            bail!("an FRD element has too many nodes");
                        }
                    } else {
                        bail!("unexpected line in the FRD element block");
                    }
                }
                finish(&mut grid, current.take(), &mut ints)?;
            }
        } else if head.starts_with("-4") {
            // result block: -4 NAME ncomps irtype, -5 component lines, data
            let tokens: Vec<&str> = head.split_ascii_whitespace().collect();
            let name = tokens.get(1).copied().unwrap_or("RESULT").to_string();
            let comps = count(tokens.get(2).copied(), "FRD component count")?;
            if comps > MAX_COMPONENTS {
                bail!("an FRD result block has too many components");
            }
            let mut stored = 0usize;
            let mut tensor = true;
            for _ in 0..comps {
                let c = lines.next().context("the FRD file is truncated")?;
                let t: Vec<&str> = c.split_ascii_whitespace().collect();
                if t.first() != Some(&"-5") {
                    bail!("component line (-5) expected in the FRD result block");
                }
                // the sixth number (iexist) is 1 for components the post-processor computes
                let computed = t.get(6).is_some_and(|s| s.starts_with('1'));
                if !computed {
                    stored += 1;
                    tensor &= t.get(3) == Some(&"4");
                }
            }
            let n = grid.point_count();
            // At most MAX_FIELDS blocks are kept (a later increment replaces
            // its block, a higher-priority block replaces a lower one); the
            // others are read and not stored, so a file with thousands of
            // differently named blocks cannot allocate a value array each.
            let slot = match fields.iter().position(|(k, _)| *k == name) {
                Some(i) => Some(i),
                None if fields.len() < MAX_FIELDS => Some(fields.len()),
                None => fields
                    .iter()
                    .rposition(|(k, _)| frd_priority(k) > frd_priority(&name)),
            };
            let mut values = if slot.is_some() {
                vec![f32::NAN; n]
            } else {
                Vec::new()
            };
            let reduce = if stored == 6 && tensor {
                Reduce::VonMises6
            } else if stored <= 1 {
                Reduce::Value
            } else {
                Reduce::Magnitude
            };
            let mut node: Option<u32> = None;
            let flush = |node: Option<u32>, buf: &mut Vec<f64>, values: &mut Vec<f32>| {
                if let Some(v) = node.and_then(|p| values.get_mut(p as usize)) {
                    if let Some(components) = buf.get(..stored).filter(|_| stored > 0) {
                        *v = reduce.apply(components);
                    }
                }
                buf.clear();
            };
            buf.clear();
            for line in lines.by_ref() {
                let tag = line.trim_start();
                if tag.starts_with("-3") {
                    break;
                } else if tag.starts_with("-1") {
                    flush(node.take(), &mut buf, &mut values);
                    let id = frd_record(line, width, stored.min(6), &mut buf)
                        .context("invalid FRD result line")?;
                    node = ids.get(&id).copied();
                } else if tag.starts_with("-2") {
                    let more = stored.saturating_sub(buf.len()).min(6);
                    frd_record(line, width, more, &mut buf).context("invalid FRD result line")?;
                } else {
                    bail!("unexpected line in the FRD result block");
                }
            }
            flush(node.take(), &mut buf, &mut values);
            if let (Some(i), true) = (slot, stored > 0 && n > 0) {
                let field = Field {
                    name: reduce.label(&name),
                    unit: String::new(),
                    values,
                };
                if let Some((_, slot)) = fields.get_mut(i).filter(|(k, _)| *k == name) {
                    *slot = field;
                } else {
                    if i < fields.len() {
                        fields.remove(i); // a lower-priority block makes room
                    }
                    fields.push((name, field));
                }
            }
        } else if head.starts_with("9999") {
            break;
        }
    }
    if !seen_nodes {
        bail!("Not a CalculiX result file (no node block).");
    }
    fields.sort_by_key(|(name, _)| frd_priority(name));
    for (_, f) in fields {
        Grid::add_field(&mut grid.point_fields, f);
    }
    grid.into_mesh()
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn calculix_element_types() {
        let expected = [
            (1, 8, HEXAHEDRON),
            (2, 6, WEDGE),
            (3, 4, TETRA),
            (4, 20, QUADRATIC_HEXAHEDRON),
            (5, 15, QUADRATIC_WEDGE),
            (6, 10, QUADRATIC_TETRA),
            (7, 3, TRIANGLE),
            (8, 6, QUADRATIC_TRIANGLE),
            (9, 4, QUAD),
            (10, 8, QUADRATIC_QUAD),
            (11, 2, LINE),
            (12, 3, QUADRATIC_EDGE),
        ];
        for (ty, nodes, vtk) in expected {
            assert_eq!(frd_element(ty), Some((nodes, vtk)), "type {ty}");
        }
        assert_eq!(frd_element(0), None);
        assert_eq!(frd_element(13), None);
    }

    #[test]
    fn fixed_width_records() {
        // " -1" + id (10 wide) + values (12 wide each), as CalculiX writes them
        let line = format!(
            " -1{:>10}{:>12}{:>12}{:>12}",
            7, "1.50000E+00", "-2.0E-01", "3.00000E+00"
        );
        let mut out = Vec::new();
        assert_eq!(frd_record(&line, 10, 3, &mut out), Some(7));
        assert_eq!(out, [1.5, -0.2, 3.0]);
        // values that touch each other only work by width
        let packed = " -1         1-1.00000E+00-2.00000E+00";
        let mut out = Vec::new();
        assert_eq!(frd_record(packed, 10, 2, &mut out), Some(1));
        assert_eq!(out, [-1.0, -2.0]);
        // a free-format line is read by tokens
        let mut out = vec![9.0];
        assert_eq!(frd_record(" -1 12 4.5 5.5", 5, 2, &mut out), Some(12));
        assert_eq!(out, [9.0, 4.5, 5.5], "appended after what was there");
        // a continuation line without an id
        let mut out = Vec::new();
        assert_eq!(frd_record(" -2 6.5", 5, 1, &mut out), Some(0));
        assert_eq!(out, [6.5]);
        // too few values: the line is refused (every caller then stops with
        // an error, so values read before the gap are never used)
        let mut out = Vec::new();
        assert_eq!(frd_record(" -1 3 4.5", 5, 3, &mut out), None);
        assert_eq!(frd_record("", 10, 1, &mut Vec::new()), None);
    }

    #[test]
    fn fixed_width_integers() {
        let mut out = Vec::new();
        assert_eq!(
            frd_ints(" -2         1         2         3", 10, &mut out),
            Some(())
        );
        assert_eq!(out, [1, 2, 3]);
        let mut out = Vec::new();
        assert_eq!(frd_ints(" -2    ", 10, &mut out), None, "no numbers");
        assert_eq!(frd_ints(" -2      abc", 10, &mut out), None, "not a number");
    }

    #[test]
    fn stress_first_then_displacements() {
        assert_eq!(frd_priority("STRESS"), 0);
        assert_eq!(frd_priority("STRESSR"), 0);
        assert_eq!(frd_priority("DISP"), 1);
        assert_eq!(frd_priority("NDTEMP"), 2);
        assert_eq!(frd_priority("stress"), 2, "names are upper case in FRD");
    }
}
