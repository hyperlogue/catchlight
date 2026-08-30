//! Derive a binding's dense evaluation grid from its sparse authored cells.
//!
//! `.clm` stores only authored keypoint cells; the dense grid the runtime
//! interpolates over (see `params.rs`) is a derived cache computed here, at
//! load (runtime) or on document change (editor). This is the single fill
//! implementation both sides call. Pixel stability depends on runtime and
//! editor deriving identical values for unauthored cells.
//!
//! The escalation order per round matches the reference: pairwise 1D
//! interpolation along each axis (second axis averages where both passes
//! computed the same cell), then parallelogram completion of 2×2 corners,
//! then constant outward extension with inverse-square-distance averaging at
//! crossings. Each round commits what it computed and repeats until dense.

/// A grid cell value the fill can mix. Deform cells are per-vertex offset
/// arrays; scalars are plain `f32`.
pub trait FillCell: Clone {
    fn lerp(a: &Self, b: &Self, t: f32) -> Self;
    fn add(&self, other: &Self) -> Self;
    fn sub(&self, other: &Self) -> Self;
}

impl FillCell for f32 {
    fn lerp(a: &Self, b: &Self, t: f32) -> Self {
        a * (1.0 - t) + b * t
    }
    fn add(&self, other: &Self) -> Self {
        self + other
    }
    fn sub(&self, other: &Self) -> Self {
        self - other
    }
}

/// Element-wise over flat offset arrays. Authored deform cells within one
/// binding always share a length; a foreign shorter cell contributes zeros
/// past its end rather than panicking.
impl FillCell for Vec<f32> {
    fn lerp(a: &Self, b: &Self, t: f32) -> Self {
        zip_longest(a, b, |x, y| x * (1.0 - t) + y * t)
    }
    fn add(&self, other: &Self) -> Self {
        zip_longest(self, other, |x, y| x + y)
    }
    fn sub(&self, other: &Self) -> Self {
        zip_longest(self, other, |x, y| x - y)
    }
}

fn zip_longest(a: &[f32], b: &[f32], f: impl Fn(f32, f32) -> f32) -> Vec<f32> {
    let n = a.len().max(b.len());
    (0..n)
        .map(|i| {
            f(
                a.get(i).copied().unwrap_or(0.0),
                b.get(i).copied().unwrap_or(0.0),
            )
        })
        .collect()
}

/// Fill the dense row-major grid (`data[y * width + x]`) from authored cells.
/// Out-of-range authored cells are ignored. Zero authored cells derive the
/// all-identity grid (an everywhere-unset binding contributes nothing).
pub fn derive_dense<T: FillCell>(
    width: usize,
    height: usize,
    axis_x: &[f32],
    axis_y: &[f32],
    authored: &[((u32, u32), T)],
    identity: &T,
) -> Vec<T> {
    let total = width * height;
    let mut values: Vec<T> = vec![identity.clone(); total];
    let mut valid = vec![false; total];
    let mut valid_count = 0usize;
    for ((x, y), v) in authored {
        let (x, y) = (*x as usize, *y as usize);
        if x < width && y < height {
            let i = y * width + x;
            if !valid[i] {
                valid_count += 1;
            }
            values[i] = v.clone();
            valid[i] = true;
        }
    }
    if valid_count == 0 || valid_count == total {
        return values;
    }

    let axis_pos = |y_major: bool, idx: usize| -> f32 {
        // Minor axis positions: pass one (y_major=false) walks along Y,
        // pass two along X — mirroring the reference's axisPoint().
        if y_major {
            axis_x.get(idx).copied().unwrap_or(idx as f32)
        } else {
            axis_y.get(idx).copied().unwrap_or(idx as f32)
        }
    };
    let index = |y_major: bool, maj: usize, min: usize| -> usize {
        if y_major {
            maj * width + min // maj = y, min = x
        } else {
            min * width + maj // maj = x, min = y
        }
    };

    let mut newly_set = vec![false; total];
    let mut interp_distance = vec![0.0f32; total];
    let mut commit: Vec<usize> = Vec::new();

    loop {
        for &i in &commit {
            if !valid[i] {
                valid[i] = true;
                valid_count += 1;
            }
        }
        commit.clear();
        if valid_count == total {
            break;
        }
        for b in newly_set.iter_mut() {
            *b = false;
        }

        let mut did_work = false;
        for second_pass in [false, true] {
            interpolate_pass(
                second_pass,
                width,
                height,
                &mut values,
                &valid,
                &mut newly_set,
                &mut commit,
                &axis_pos,
                &index,
            );
        }
        if !commit.is_empty() {
            did_work = true;
        }

        if !did_work {
            extrapolate_corners(width, height, &mut values, &valid, &mut commit);
            did_work = !commit.is_empty();
        }

        if !did_work {
            for second_pass in [false, true] {
                extend_pass(
                    second_pass,
                    width,
                    height,
                    &mut values,
                    &valid,
                    &mut newly_set,
                    &mut interp_distance,
                    &mut commit,
                    &axis_pos,
                    &index,
                );
            }
            did_work = !commit.is_empty();
        }

        if !did_work {
            break;
        }
    }
    values
}

#[allow(clippy::too_many_arguments)]
fn interpolate_pass<T: FillCell>(
    second_pass: bool,
    width: usize,
    height: usize,
    values: &mut [T],
    valid: &[bool],
    newly_set: &mut [bool],
    commit: &mut Vec<usize>,
    axis_pos: &impl Fn(bool, usize) -> f32,
    index: &impl Fn(bool, usize, usize) -> usize,
) {
    let (major_cnt, minor_cnt) = if second_pass {
        (height, width)
    } else {
        (width, height)
    };
    let mut detected_intersections = false;
    for i in 0..major_cnt {
        let mut l = 0usize;
        let cnt = minor_cnt;
        while l < cnt && !valid[index(second_pass, i, l)] {
            l += 1;
        }
        if l >= cnt {
            continue;
        }
        loop {
            while l < cnt - 1 && valid[index(second_pass, i, l + 1)] {
                l += 1;
            }
            if l >= cnt - 1 {
                break;
            }
            let mut r = l + 1;
            while r < cnt && !valid[index(second_pass, i, r)] {
                r += 1;
            }
            if r >= cnt {
                break;
            }
            let left_off = axis_pos(second_pass, l);
            let right_off = axis_pos(second_pass, r);
            for m in (l + 1)..r {
                let mid_off = axis_pos(second_pass, m);
                let t = if (right_off - left_off).abs() > f32::EPSILON {
                    (mid_off - left_off) / (right_off - left_off)
                } else {
                    0.5
                };
                let val = T::lerp(
                    &values[index(second_pass, i, l)],
                    &values[index(second_pass, i, r)],
                    t,
                );
                let idx = index(second_pass, i, m);
                if second_pass && newly_set[idx] {
                    if !detected_intersections {
                        commit.clear();
                    }
                    values[idx] = T::lerp(&val, &values[idx], 0.5);
                    commit.push(idx);
                    detected_intersections = true;
                } else if !detected_intersections {
                    values[idx] = val;
                    newly_set[idx] = true;
                    commit.push(idx);
                }
            }
            l = r;
        }
    }
}

fn extrapolate_corners<T: FillCell>(
    width: usize,
    height: usize,
    values: &mut [T],
    valid: &[bool],
    commit: &mut Vec<usize>,
) {
    if width <= 1 || height <= 1 {
        return;
    }
    let at = |x: usize, y: usize| y * width + x;
    // Signed offsets: the missing corner is completed as base + dX + dY.
    let mut complete = |bx: i64, by: i64, ox: i64, oy: i64, values: &mut [T]| {
        let base = values[at(bx as usize, by as usize)].clone();
        let vx = values[at((bx + ox) as usize, by as usize)].clone();
        let vy = values[at(bx as usize, (by + oy) as usize)].clone();
        let idx = at((bx + ox) as usize, (by + oy) as usize);
        values[idx] = vx.add(&vy).sub(&base);
        commit.push(idx);
    };
    for x in 0..width - 1 {
        for y in 0..height - 1 {
            let v00 = valid[at(x, y)];
            let v10 = valid[at(x + 1, y)];
            let v01 = valid[at(x, y + 1)];
            let v11 = valid[at(x + 1, y + 1)];
            let (x, y) = (x as i64, y as i64);
            if v00 && v10 && v01 && !v11 {
                complete(x, y, 1, 1, values);
            } else if v00 && v10 && !v01 && v11 {
                complete(x + 1, y, -1, 1, values);
            } else if v00 && !v10 && v01 && v11 {
                complete(x, y + 1, 1, -1, values);
            } else if !v00 && v10 && v01 && v11 {
                complete(x + 1, y + 1, -1, -1, values);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn extend_pass<T: FillCell>(
    second_pass: bool,
    width: usize,
    height: usize,
    values: &mut [T],
    valid: &[bool],
    newly_set: &mut [bool],
    interp_distance: &mut [f32],
    commit: &mut Vec<usize>,
    axis_pos: &impl Fn(bool, usize) -> f32,
    index: &impl Fn(bool, usize, usize) -> usize,
) {
    let (major_cnt, minor_cnt) = if second_pass {
        (height, width)
    } else {
        (width, height)
    };
    let mut detected_intersections = false;
    for i in 0..major_cnt {
        let cnt = minor_cnt;
        let mut j = 0usize;
        while j < cnt && !valid[index(second_pass, i, j)] {
            j += 1;
        }
        if j >= cnt {
            continue;
        }

        let mut set_or_average =
            |min: usize, val: &T, origin: f32, values: &mut [T], di: &mut bool| {
                let idx = index(second_pass, i, min);
                let min_dist = (axis_pos(second_pass, min) - origin).abs();
                if second_pass && newly_set[idx] {
                    if !*di {
                        commit.clear();
                    }
                    let maj_dist = interp_distance[idx];
                    let denom = min_dist + maj_dist * maj_dist / min_dist.max(f32::EPSILON);
                    let frac = if denom > f32::EPSILON {
                        min_dist / denom
                    } else {
                        0.5
                    };
                    values[idx] = T::lerp(val, &values[idx], frac);
                    commit.push(idx);
                    *di = true;
                }
                if !*di {
                    values[idx] = val.clone();
                    interp_distance[idx] = min_dist;
                    newly_set[idx] = true;
                    commit.push(idx);
                }
            };

        let first = values[index(second_pass, i, j)].clone();
        let origin = axis_pos(second_pass, j);
        for k in 0..j {
            set_or_average(k, &first, origin, values, &mut detected_intersections);
        }

        let mut j2 = cnt - 1;
        while !valid[index(second_pass, i, j2)] {
            j2 -= 1;
        }
        let last = values[index(second_pass, i, j2)].clone();
        let origin = axis_pos(second_pass, j2);
        for k in (j2 + 1)..cnt {
            set_or_average(k, &last, origin, values, &mut detected_intersections);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid(w: usize, h: usize, cells: &[((u32, u32), f32)]) -> Vec<f32> {
        derive_dense(w, h, &pos(w), &pos(h), cells, &0.0)
    }

    fn pos(n: usize) -> Vec<f32> {
        (0..n).map(|i| i as f32).collect()
    }

    #[test]
    fn all_authored_is_identity_projection() {
        let cells: Vec<((u32, u32), f32)> = (0..6)
            .map(|i| (((i % 3) as u32, (i / 3) as u32), i as f32))
            .collect();
        let out = grid(3, 2, &cells);
        assert_eq!(out, vec![0.0, 1.0, 2.0, 3.0, 4.0, 5.0]);
    }

    #[test]
    fn empty_authored_fills_identity() {
        assert_eq!(grid(3, 1, &[]), vec![0.0; 3]);
        let deform = derive_dense(2, 1, &[0.0, 1.0], &[0.0], &[], &vec![0.0f32; 4]);
        assert_eq!(deform, vec![vec![0.0; 4], vec![0.0; 4]]);
    }

    #[test]
    fn one_d_interpolates_between_and_extends_past_endpoints() {
        // axis 0..4, authored at 1 and 3.
        let out = grid(5, 1, &[((1, 0), 10.0), ((3, 0), 30.0)]);
        // between: linear; outside: constant extension (reference semantics).
        assert_eq!(out, vec![10.0, 10.0, 20.0, 30.0, 30.0]);
    }

    #[test]
    fn non_uniform_axis_positions_weight_the_lerp() {
        let out = derive_dense(
            3,
            1,
            &[0.0, 0.25, 1.0],
            &[0.0],
            &[((0, 0), 0.0f32), ((2, 0), 1.0)],
            &0.0,
        );
        assert!((out[1] - 0.25).abs() < 1e-6);
    }

    #[test]
    fn corner_completion_is_parallelogram() {
        // 2x2 with 3 corners authored: missing corner = vx + vy - base.
        let out = grid(2, 2, &[((0, 0), 1.0), ((1, 0), 3.0), ((0, 1), 5.0)]);
        assert_eq!(out[3], 3.0 + 5.0 - 1.0);
    }

    #[test]
    fn single_cell_extends_everywhere() {
        let out = grid(3, 3, &[((1, 1), 7.0)]);
        assert!(out.iter().all(|&v| (v - 7.0).abs() < 1e-6));
    }

    #[test]
    fn cross_axis_interpolation_averages_at_intersections() {
        // A plus-shape: authored at the four edge midpoints of a 3x3.
        let out = grid(
            3,
            3,
            &[((1, 0), 0.0), ((0, 1), 2.0), ((2, 1), 4.0), ((1, 2), 6.0)],
        );
        // center is interpolated by both passes: x-pass gives 3, y-pass 3 → 3.
        assert!((out[4] - 3.0).abs() < 1e-6);
    }

    #[test]
    fn deform_cells_fill_elementwise() {
        let out = derive_dense(
            3,
            1,
            &[0.0, 1.0, 2.0],
            &[0.0],
            &[((0, 0), vec![0.0f32, 0.0]), ((2, 0), vec![2.0, -4.0])],
            &vec![0.0f32; 2],
        );
        assert_eq!(out[1], vec![1.0, -2.0]);
    }
}
