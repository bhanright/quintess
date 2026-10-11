//! Editing the pictures and drawings on a page: finding them, then moving,
//! resizing, rotating, flipping, cropping, replacing or deleting them.
//!
//! Each object is a run of operators in the page's content stream: an
//! image or form drawn with `Do`, an inline image, or a path that is built
//! and painted. Changes touch only that run: a transformation is added by
//! wrapping the run in `q <matrix> cm … Q`, a crop by wrapping it in a
//! clipping path, and a deletion removes the run. Everything else on the
//! page, text above all, keeps its exact bytes.
//!
//! Page space has its origin at the bottom left, y up, in points.

use anyhow::{Context, Result, bail};
use lopdf::content::{Content, Operation};
use lopdf::{Dictionary, Object, ObjectId, Stream};
use serde::{Deserialize, Serialize};

use crate::document::Pdf;
use crate::redact::encode_operations;

/// An affine matrix `[a, b, c, d, e, f]`, as PDF writes them: a point
/// (x, y) maps to (a·x + c·y + e, b·x + d·y + f).
pub type Matrix = [f32; 6];

pub const IDENTITY: Matrix = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

/// `first` then `second`.
pub fn multiply(first: Matrix, second: Matrix) -> Matrix {
    let [a, b, c, d, e, f] = first;
    let [a2, b2, c2, d2, e2, f2] = second;
    [a * a2 + b * c2, a * b2 + b * d2, c * a2 + d * c2, c * b2 + d * d2, e * a2 + f * c2 + e2, e * b2 + f * d2 + f2]
}

pub fn invert(matrix: Matrix) -> Option<Matrix> {
    let [a, b, c, d, e, f] = matrix;
    let determinant = a * d - b * c;
    if determinant.abs() < 1e-9 {
        return None;
    }
    let (ia, ib, ic, id) = (d / determinant, -b / determinant, -c / determinant, a / determinant);
    Some([ia, ib, ic, id, -(e * ia + f * ic), -(e * ib + f * id)])
}

pub fn apply(matrix: Matrix, (x, y): (f32, f32)) -> (f32, f32) {
    (matrix[0] * x + matrix[2] * y + matrix[4], matrix[1] * x + matrix[3] * y + matrix[5])
}

/// The bounding box of points, `[left, bottom, right, top]`.
fn bounds_of(points: impl IntoIterator<Item = (f32, f32)>) -> Option<[f32; 4]> {
    let mut bounds: Option<[f32; 4]> = None;
    for (x, y) in points {
        bounds = Some(match bounds {
            None => [x, y, x, y],
            Some([l, b, r, t]) => [l.min(x), b.min(y), r.max(x), t.max(y)],
        });
    }
    bounds
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum ObjectKind {
    /// A picture (an image XObject or an inline image).
    Image,
    /// A drawing: lines, curves, filled shapes.
    Shape,
    /// A group drawn as one piece (a form XObject), such as a logo or chart.
    Group,
}

impl ObjectKind {
    pub fn describe(self) -> &'static str {
        match self {
            ObjectKind::Image => "Picture",
            ObjectKind::Shape => "Shape",
            ObjectKind::Group => "Group",
        }
    }
}

/// A picture or drawing on a page.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PageObject {
    pub kind: ObjectKind,
    /// The operators that draw it: `operations[start..end]` of the page's
    /// decoded content. Valid until the page changes.
    pub start: usize,
    pub end: usize,
    /// Where it appears, in page space.
    pub bounds: [f32; 4],
    /// The transformation in force when it is drawn (user space to page).
    pub ctm: Matrix,
    /// For images and groups: the XObject's resource name.
    pub name: Option<String>,
    /// For images: pixel size.
    pub pixels: Option<(u32, u32)>,
    /// How solid it is drawn (1 solid, 0 invisible).
    pub opacity: f32,
}

fn number(object: &Object) -> Option<f32> {
    match object {
        Object::Integer(value) => Some(*value as f32),
        Object::Real(value) => Some(*value),
        _ => None,
    }
}

fn numbers(operation: &Operation) -> Vec<f32> {
    operation.operands.iter().filter_map(number).collect()
}

const PAINT_OPERATORS: [&str; 10] = ["S", "s", "f", "F", "f*", "B", "B*", "b", "b*", "n"];
const PATH_OPERATORS: [&str; 7] = ["m", "l", "c", "v", "y", "re", "h"];

/// The page's content, decoded into one list of operators.
fn page_operations(pdf: &Pdf, page_id: ObjectId) -> Result<Vec<Operation>> {
    let content = pdf.doc.get_page_content(page_id);
    Ok(Content::decode(&content).context("the page's content couldn't be read")?.operations)
}

/// Every picture and drawing on a page (1-based `page_number`), in drawing
/// order (later ones are on top).
pub fn list_objects(pdf: &Pdf, page_number: u32) -> Result<Vec<PageObject>> {
    let page_id = pdf.page_id(page_number)?;
    let operations = page_operations(pdf, page_id)?;
    let xobjects = page_xobjects(pdf, page_id);
    let states = page_resource_category(pdf, page_id, b"ExtGState");
    Ok(find_objects(pdf, &operations, &xobjects, &states))
}

/// The page's XObject dictionary, resolved.
pub(crate) fn page_xobjects(pdf: &Pdf, page_id: ObjectId) -> Dictionary {
    page_resource_category(pdf, page_id, b"XObject")
}

fn page_resource_category(pdf: &Pdf, page_id: ObjectId, category: &[u8]) -> Dictionary {
    let Ok(Some(resources)) = pdf.inherited_attribute(page_id, b"Resources") else { return Dictionary::new() };
    let resources = pdf.resolve(&resources).as_dict().cloned().unwrap_or_default();
    resources.get(category).ok().and_then(|x| pdf.resolve(x).as_dict().ok()).cloned().unwrap_or_default()
}

fn find_objects(pdf: &Pdf, operations: &[Operation], xobjects: &Dictionary, states: &Dictionary) -> Vec<PageObject> {
    let mut found = Vec::new();
    let mut ctm = IDENTITY;
    // The fill opacity in force (from `gs` with /ca), saved with the CTM.
    let mut opacity: f32 = 1.0;
    let mut stack: Vec<(Matrix, f32)> = Vec::new();
    // The path being built: where it starts, its points (user space), and
    // whether it is used as a clip.
    let mut path_start: Option<usize> = None;
    let mut path_points: Vec<(f32, f32)> = Vec::new();
    let mut clipping = false;
    let mut in_text = false;
    for (index, operation) in operations.iter().enumerate() {
        let operator = operation.operator.as_str();
        match operator {
            "q" => stack.push((ctm, opacity)),
            "Q" => (ctm, opacity) = stack.pop().unwrap_or((IDENTITY, 1.0)),
            "gs" => {
                let state = operation.operands.first().and_then(|o| o.as_name().ok()).and_then(|name| states.get(name).ok()).and_then(|s| pdf.resolve(s).as_dict().ok());
                if let Some(alpha) = state.and_then(|s| s.get(b"ca").ok()).and_then(|a| pdf.resolve(a).as_float().ok()) {
                    opacity = alpha;
                }
            }
            "cm" => {
                let values = numbers(operation);
                if values.len() == 6 {
                    ctm = multiply([values[0], values[1], values[2], values[3], values[4], values[5]], ctm);
                }
            }
            "BT" => in_text = true,
            "ET" => in_text = false,
            _ if in_text => {}
            _ if PATH_OPERATORS.contains(&operator) => {
                path_start.get_or_insert(index);
                let values = numbers(operation);
                match operator {
                    "re" if values.len() == 4 => {
                        let (x, y, w, h) = (values[0], values[1], values[2], values[3]);
                        path_points.extend([(x, y), (x + w, y), (x + w, y + h), (x, y + h)]);
                    }
                    _ => path_points.extend(values.chunks_exact(2).map(|pair| (pair[0], pair[1]))),
                }
            }
            "W" | "W*" => clipping = true,
            _ if PAINT_OPERATORS.contains(&operator) => {
                if let Some(start) = path_start.take() {
                    // Clipping paths aren't drawings; changing them would
                    // change what everything after them shows.
                    if operator != "n" && !clipping {
                        if let Some(bounds) = bounds_of(path_points.iter().map(|point| apply(ctm, *point))) {
                            found.push(PageObject { kind: ObjectKind::Shape, start, end: index + 1, bounds, ctm, name: None, pixels: None, opacity });
                        }
                    }
                }
                path_points.clear();
                clipping = false;
            }
            "Do" => {
                let Some(name) = operation.operands.first().and_then(|o| o.as_name().ok()) else { continue };
                let Some(stream) = xobjects.get(name).ok().and_then(|o| pdf.resolve(o).as_stream().ok()) else { continue };
                let subtype = stream.dict.get(b"Subtype").ok().and_then(|s| s.as_name().ok()).unwrap_or(b"");
                let unit_square = [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)];
                let (kind, bounds, pixels) = if subtype == b"Image" {
                    let pixels = (stream.dict.get(b"Width").ok().and_then(|w| w.as_i64().ok()).unwrap_or(0) as u32, stream.dict.get(b"Height").ok().and_then(|h| h.as_i64().ok()).unwrap_or(0) as u32);
                    (ObjectKind::Image, bounds_of(unit_square.map(|p| apply(ctm, p))), Some(pixels))
                } else {
                    // A form: its BBox, through its own matrix and the CTM.
                    let bbox: Vec<f32> = stream.dict.get(b"BBox").ok().and_then(|b| pdf.resolve(b).as_array().ok()).map(|items| items.iter().filter_map(|i| number(pdf.resolve(i))).collect()).unwrap_or_default();
                    let form_matrix: Vec<f32> = stream.dict.get(b"Matrix").ok().and_then(|m| pdf.resolve(m).as_array().ok()).map(|items| items.iter().filter_map(|i| number(pdf.resolve(i))).collect()).unwrap_or_default();
                    let form_matrix = if form_matrix.len() == 6 { [form_matrix[0], form_matrix[1], form_matrix[2], form_matrix[3], form_matrix[4], form_matrix[5]] } else { IDENTITY };
                    if bbox.len() != 4 {
                        continue;
                    }
                    let corners = [(bbox[0], bbox[1]), (bbox[2], bbox[1]), (bbox[2], bbox[3]), (bbox[0], bbox[3])];
                    let to_page = multiply(form_matrix, ctm);
                    (ObjectKind::Group, bounds_of(corners.map(|p| apply(to_page, p))), None)
                };
                if let Some(bounds) = bounds {
                    found.push(PageObject { kind, start: index, end: index + 1, bounds, ctm, name: Some(String::from_utf8_lossy(name).into_owned()), pixels, opacity });
                }
            }
            "BI" => {
                let unit_square = [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)];
                if let Some(bounds) = bounds_of(unit_square.map(|p| apply(ctm, p))) {
                    found.push(PageObject { kind: ObjectKind::Image, start: index, end: index + 1, bounds, ctm, name: None, pixels: None, opacity });
                }
            }
            _ => {}
        }
    }
    // Ignore specks and full-page backgrounds drawn as a single rectangle
    // only if they are degenerate; everything else can be picked.
    found.retain(|object| object.bounds[2] - object.bounds[0] > 0.01 || object.bounds[3] - object.bounds[1] > 0.01);
    found
}

/// The topmost object under a point, preferring pictures and groups over
/// shapes drawn beneath them.
pub fn object_at(objects: &[PageObject], x: f32, y: f32) -> Option<&PageObject> {
    let tolerance = 2.0;
    objects.iter().rev().find(|object| {
        let [l, b, r, t] = object.bounds;
        x >= l - tolerance && x <= r + tolerance && y >= b - tolerance && y <= t + tolerance
    })
}

/// Every object lying wholly inside `area` (for selecting with a box).
pub fn objects_in(objects: &[PageObject], area: [f32; 4]) -> Vec<PageObject> {
    objects.iter().filter(|object| object.bounds[0] >= area[0] && object.bounds[1] >= area[1] && object.bounds[2] <= area[2] && object.bounds[3] <= area[3]).cloned().collect()
}

/// A change to make to selected objects.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum Change {
    /// A transformation in page space, applied after the object's own.
    Transform(Matrix),
    /// Shows only the part inside this page-space rectangle.
    Crop([f32; 4]),
    Delete,
    /// Draws a different picture in the same place (images only), fitted
    /// inside the old box keeping its proportions.
    ReplaceImage(Vec<u8>),
    /// Moves to the front or back of the page's drawing order.
    BringToFront,
    SendToBack,
    /// One step up or down the page's drawing order, past the next object
    /// that overlaps it.
    BringForward,
    SendBackward,
    /// See-through drawing (0 invisible to 1 solid) and how colors mix
    /// with what is underneath (a PDF blend mode such as "Multiply").
    Appearance { opacity: f32, blend_mode: BlendMode },
}

/// How an object's colors combine with what lies beneath, as in image
/// editors. The names are PDF's own.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum BlendMode {
    Normal,
    Multiply,
    Screen,
    Overlay,
    Darken,
    Lighten,
    ColorDodge,
    ColorBurn,
    HardLight,
    SoftLight,
    Difference,
    Exclusion,
    Hue,
    Saturation,
    Color,
    Luminosity,
}

impl BlendMode {
    pub const ALL: [BlendMode; 16] = [BlendMode::Normal, BlendMode::Multiply, BlendMode::Screen, BlendMode::Overlay, BlendMode::Darken, BlendMode::Lighten, BlendMode::ColorDodge, BlendMode::ColorBurn, BlendMode::HardLight, BlendMode::SoftLight, BlendMode::Difference, BlendMode::Exclusion, BlendMode::Hue, BlendMode::Saturation, BlendMode::Color, BlendMode::Luminosity];

    /// The PDF name, which is also the label people know from image editors.
    pub fn name(self) -> &'static str {
        match self {
            BlendMode::Normal => "Normal",
            BlendMode::Multiply => "Multiply",
            BlendMode::Screen => "Screen",
            BlendMode::Overlay => "Overlay",
            BlendMode::Darken => "Darken",
            BlendMode::Lighten => "Lighten",
            BlendMode::ColorDodge => "ColorDodge",
            BlendMode::ColorBurn => "ColorBurn",
            BlendMode::HardLight => "HardLight",
            BlendMode::SoftLight => "SoftLight",
            BlendMode::Difference => "Difference",
            BlendMode::Exclusion => "Exclusion",
            BlendMode::Hue => "Hue",
            BlendMode::Saturation => "Saturation",
            BlendMode::Color => "Color",
            BlendMode::Luminosity => "Luminosity",
        }
    }
}

/// Page-space transformations, for use with [`Change::Transform`].
pub fn translation(dx: f32, dy: f32) -> Matrix {
    [1.0, 0.0, 0.0, 1.0, dx, dy]
}

/// Scales about a fixed point (such as the opposite corner when resizing).
pub fn scaling_about(sx: f32, sy: f32, (x, y): (f32, f32)) -> Matrix {
    [sx, 0.0, 0.0, sy, x - sx * x, y - sy * y]
}

/// Rotates counterclockwise by `degrees` about a point.
pub fn rotation_about(degrees: f32, (x, y): (f32, f32)) -> Matrix {
    let (sin, cos) = degrees.to_radians().sin_cos();
    multiply(multiply(translation(-x, -y), [cos, sin, -sin, cos, 0.0, 0.0]), translation(x, y))
}

/// Maps the rectangle `from` onto `to` (both `[left, bottom, right, top]`).
pub fn rectangle_to_rectangle(from: [f32; 4], to: [f32; 4]) -> Matrix {
    let sx = (to[2] - to[0]) / (from[2] - from[0]).max(0.001);
    let sy = (to[3] - to[1]) / (from[3] - from[1]).max(0.001);
    [sx, 0.0, 0.0, sy, to[0] - sx * from[0], to[1] - sy * from[1]]
}

fn real(value: f32) -> Object {
    Object::Real((value * 10000.0).round() / 10000.0)
}

fn matrix_operation(matrix: Matrix) -> Operation {
    Operation::new("cm", matrix.iter().map(|v| real(*v)).collect())
}

/// Applies `change` to each of `selected` on a page. The objects must come
/// from [`list_objects`] on the same page, unchanged since.
pub fn change_objects(pdf: &mut Pdf, page_number: u32, selected: &[PageObject], change: &Change) -> Result<()> {
    if selected.is_empty() {
        bail!("nothing is selected");
    }
    let page_id = pdf.page_id(page_number)?;
    pdf.materialize_inherited_attributes(page_id)?;
    let mut operations = page_operations(pdf, page_id)?;
    // Check the selection still matches the page.
    let current = find_objects(pdf, &operations, &page_xobjects(pdf, page_id), &page_resource_category(pdf, page_id, b"ExtGState"));
    for object in selected {
        if !current.iter().any(|known| known.start == object.start && known.end == object.end && known.kind == object.kind) {
            bail!("the page has changed since these objects were picked; pick them again");
        }
    }
    let mut ordered: Vec<&PageObject> = selected.iter().collect();
    // From the end of the page backwards, so earlier positions stay valid.
    ordered.sort_by(|a, b| b.start.cmp(&a.start));
    match change {
        Change::Delete => {
            for object in &ordered {
                operations.drain(object.start..object.end);
            }
        }
        Change::Transform(transform) => {
            for object in &ordered {
                // In the object's user space: C × T × C⁻¹, so the page-space
                // transformation T happens after its own.
                let inverse = invert(object.ctm).context("this object is drawn with a flat transformation and can't be changed")?;
                let local = multiply(multiply(object.ctm, *transform), inverse);
                wrap(&mut operations, object, vec![Operation::new("q", vec![]), matrix_operation(local)]);
            }
        }
        Change::Crop(area) => {
            for object in &ordered {
                let inverse = invert(object.ctm).context("this object can't be cropped")?;
                let corners = [(area[0], area[1]), (area[2], area[1]), (area[2], area[3]), (area[0], area[3])].map(|p| apply(inverse, p));
                let mut before = vec![Operation::new("q", vec![]), Operation::new("m", vec![real(corners[0].0), real(corners[0].1)])];
                for corner in &corners[1..] {
                    before.push(Operation::new("l", vec![real(corner.0), real(corner.1)]));
                }
                before.extend([Operation::new("h", vec![]), Operation::new("W", vec![]), Operation::new("n", vec![])]);
                wrap(&mut operations, object, before);
            }
        }
        Change::ReplaceImage(bytes) => {
            let embedded = crate::images::embed_image_bytes(&mut pdf.doc, bytes)?;
            let name = pdf.add_page_resource(page_id, b"XObject", "QsIm", Object::Reference(embedded.id))?;
            for object in &ordered {
                if object.kind != ObjectKind::Image {
                    bail!("only pictures can be replaced");
                }
                // Fit the new picture's proportions inside the old box.
                let (width, height) = (object.bounds[2] - object.bounds[0], object.bounds[3] - object.bounds[1]);
                let picture_ratio = embedded.width as f32 / embedded.height.max(1) as f32;
                let (fit_width, fit_height) = if width / height.max(0.001) > picture_ratio { (height * picture_ratio, height) } else { (width, width / picture_ratio) };
                let left = object.bounds[0] + (width - fit_width) / 2.0;
                let bottom = object.bounds[1] + (height - fit_height) / 2.0;
                let inverse = invert(object.ctm).context("this picture can't be replaced")?;
                // Draw the unit square at the fitted box in page space.
                let placement = multiply([fit_width, 0.0, 0.0, fit_height, left, bottom], inverse);
                let replacement = vec![Operation::new("q", vec![]), matrix_operation(placement), Operation::new("Do", vec![Object::Name(name.as_bytes().to_vec())]), Operation::new("Q", vec![])];
                operations.splice(object.start..object.end, replacement);
            }
        }
        Change::Appearance { opacity, blend_mode } => {
            let opacity = opacity.clamp(0.0, 1.0);
            let state = lopdf::dictionary! { "Type" => "ExtGState", "ca" => opacity, "CA" => opacity, "BM" => Object::Name(blend_mode.name().as_bytes().to_vec()) };
            let name = pdf.add_page_resource(page_id, b"ExtGState", "QsGs", Object::Dictionary(state))?;
            for object in &ordered {
                // When an earlier opacity change already wraps the object,
                // replace its setting so the new value is absolute, not
                // stacked on the old one.
                let earlier = object.start >= 2
                    && operations[object.start - 2].operator == "q"
                    && operations[object.start - 1].operator == "gs"
                    && operations[object.start - 1].operands.first().and_then(|o| o.as_name().ok()).is_some_and(|n| n.starts_with(b"QsGs"))
                    && operations.get(object.end).is_some_and(|o| o.operator == "Q");
                if earlier {
                    operations[object.start - 1] = Operation::new("gs", vec![Object::Name(name.as_bytes().to_vec())]);
                } else {
                    wrap(&mut operations, object, vec![Operation::new("q", vec![]), Operation::new("gs", vec![Object::Name(name.as_bytes().to_vec())])]);
                }
            }
        }
        Change::BringForward | Change::SendBackward => {
            if ordered.len() != 1 {
                bail!("move one object at a time up or down the order");
            }
            let object = ordered[0];
            let overlaps = |other: &PageObject| other.bounds[0] < object.bounds[2] && other.bounds[2] > object.bounds[0] && other.bounds[1] < object.bounds[3] && other.bounds[3] > object.bounds[1];
            let neighbor = if *change == Change::BringForward {
                current.iter().filter(|other| other.start >= object.end && overlaps(other)).min_by_key(|other| other.start)
            } else {
                current.iter().filter(|other| other.end <= object.start && overlaps(other)).max_by_key(|other| other.start)
            };
            let Some(neighbor) = neighbor.cloned() else { bail!("nothing overlapping is {} it", if *change == Change::BringForward { "in front of" } else { "behind" }) };
            // Lift the object with its look, expressed relative to the
            // transformation in force where it lands (the neighbor's).
            let landing = invert(neighbor.ctm).context("the neighboring object can't be used as a reference")?;
            let mut piece = vec![Operation::new("q", vec![]), matrix_operation(multiply(object.ctm, landing))];
            piece.extend(state_before(&operations, object.start));
            let lifted: Vec<Operation> = operations[object.start..object.end].to_vec();
            piece.extend(lifted);
            piece.push(Operation::new("Q", vec![]));
            let length = object.end - object.start;
            if *change == Change::BringForward {
                // Insert after the neighbor first, then remove the original
                // (which comes before it, so the neighbor's position holds).
                for (offset, operation) in piece.into_iter().enumerate() {
                    operations.insert(neighbor.end + offset, operation);
                }
                operations.drain(object.start..object.start + length);
            } else {
                operations.drain(object.start..object.end);
                for (offset, operation) in piece.into_iter().enumerate() {
                    operations.insert(neighbor.start + offset, operation);
                }
            }
        }
        Change::BringToFront | Change::SendToBack => {
            // Lift each object out with the graphics state it was drawn in
            // (its transformation; colors and line styles set before it
            // travel with the operators that set them only when inside it).
            let mut lifted = Vec::new();
            for object in &ordered {
                let mut piece = vec![Operation::new("q", vec![]), matrix_operation(object.ctm)];
                // Colors and line settings in force at the object.
                piece.extend(state_before(&operations, object.start));
                piece.extend(operations.drain(object.start..object.end));
                piece.push(Operation::new("Q", vec![]));
                lifted.push(piece);
            }
            lifted.reverse();
            let lifted: Vec<Operation> = lifted.into_iter().flatten().collect();
            if *change == Change::BringToFront {
                // Close any open graphics states first, so the lifted pieces
                // start from the page's own coordinate system.
                let open = open_states(&operations);
                operations.extend(std::iter::repeat_n(Operation::new("Q", vec![]), open));
                operations.extend(lifted);
            } else {
                let mut front = lifted;
                front.extend(operations);
                operations = front;
            }
        }
    }
    let content_id = pdf.doc.add_object(Stream::new(Dictionary::new(), encode_operations(&operations)));
    pdf.doc.get_dictionary_mut(page_id)?.set("Contents", content_id);
    Ok(())
}

/// Surrounds an object's operators with `before` and a closing `Q`.
fn wrap(operations: &mut Vec<Operation>, object: &PageObject, before: Vec<Operation>) {
    operations.insert(object.end, Operation::new("Q", vec![]));
    for (offset, operation) in before.into_iter().enumerate() {
        operations.insert(object.start + offset, operation);
    }
}

/// How many `q` are still open at the end of the operators.
fn open_states(operations: &[Operation]) -> usize {
    let mut depth: usize = 0;
    for operation in operations {
        match operation.operator.as_str() {
            "q" => depth += 1,
            "Q" => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    depth
}

/// The color, line and graphics-state operators in force at `index`, so a
/// moved object keeps its look.
fn state_before(operations: &[Operation], index: usize) -> Vec<Operation> {
    const STATE: [&str; 17] = ["w", "J", "j", "M", "d", "gs", "CS", "cs", "SC", "SCN", "sc", "scn", "G", "g", "RG", "rg", "K"];
    let mut stack: Vec<Vec<Operation>> = vec![Vec::new()];
    for operation in &operations[..index] {
        match operation.operator.as_str() {
            "q" => {
                let top = stack.last().cloned().unwrap_or_default();
                stack.push(top);
            }
            "Q" => {
                if stack.len() > 1 {
                    stack.pop();
                }
            }
            operator if STATE.contains(&operator) || operator == "k" => stack.last_mut().expect("never empty").push(operation.clone()),
            _ => {}
        }
    }
    stack.pop().unwrap_or_default()
}

/// Adds a picture to a page, drawn in the page-space rectangle `area`.
pub fn add_image(pdf: &mut Pdf, page_number: u32, bytes: &[u8], area: [f32; 4]) -> Result<()> {
    let page_id = pdf.page_id(page_number)?;
    let embedded = crate::images::embed_image_bytes(&mut pdf.doc, bytes)?;
    let name = pdf.add_page_resource(page_id, b"XObject", "QsIm", Object::Reference(embedded.id))?;
    let snippet = crate::images::draw_image_operator(&name, area[0], area[1], area[2] - area[0], area[3] - area[1]);
    pdf.add_page_content(page_id, snippet.into_bytes(), true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matrices_compose_and_invert() {
        let m = multiply(scaling_about(2.0, 3.0, (0.0, 0.0)), translation(5.0, 7.0));
        assert_eq!(apply(m, (1.0, 1.0)), (7.0, 10.0));
        let back = invert(m).unwrap();
        let (x, y) = apply(back, (7.0, 10.0));
        assert!((x - 1.0).abs() < 1e-5 && (y - 1.0).abs() < 1e-5);
        let (x, y) = apply(rotation_about(90.0, (1.0, 1.0)), (2.0, 1.0));
        assert!((x - 1.0).abs() < 1e-5 && (y - 2.0).abs() < 1e-5);
    }
}
