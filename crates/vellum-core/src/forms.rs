//! Interactive forms (AcroForm): list, fill, create, detect, export and
//! import field data, and flatten annotations and fields into page content.

use std::collections::{BTreeMap, HashSet};

use anyhow::{Result, anyhow, bail};
use lopdf::{Dictionary, Object, ObjectId, Stream, dictionary};
use serde::{Deserialize, Serialize};

use crate::document::{Pdf, page_numbers_by_id, rectangle_from_object, rectangle_to_object};
use crate::fonts::StandardFont;
use crate::pages::number;
use crate::strings::{object_to_text, text_string};
use crate::unicode_text::TextSetter;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum FieldType {
    Text,
    CheckBox,
    RadioGroup,
    PushButton,
    ComboBox,
    ListBox,
    Signature,
    Unknown,
}

/// A form field as a person sees it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FieldInfo {
    /// The fully qualified name, parts joined with dots.
    pub name: String,
    pub field_type: FieldType,
    pub value: String,
    /// Choices for combo boxes, list boxes and radio groups (their on-states).
    pub options: Vec<String>,
    pub required: bool,
    pub read_only: bool,
    pub multiline: bool,
    pub max_length: Option<u32>,
    /// Tooltip text, often the human-readable label.
    pub tooltip: String,
    /// Each widget: 1-based page and rectangle in page space.
    pub widgets: Vec<(u32, [f32; 4])>,
    #[serde(skip)]
    pub id: Option<ObjectId>,
}

const FLAG_READ_ONLY: i64 = 1;
const FLAG_REQUIRED: i64 = 1 << 1;
const FLAG_MULTILINE: i64 = 1 << 12;
const FLAG_RADIO: i64 = 1 << 15;
const FLAG_PUSH_BUTTON: i64 = 1 << 16;
const FLAG_COMBO: i64 = 1 << 17;

fn acroform(pdf: &Pdf) -> Option<Dictionary> {
    let catalog = pdf.doc.catalog().ok()?;
    pdf.resolve(catalog.get(b"AcroForm").ok()?).as_dict().ok().cloned()
}

/// Looks a key up on a field or its ancestors (FT, Ff, V, DA and Opt inherit).
fn inherited<'a>(pdf: &'a Pdf, field: &'a Dictionary, key: &[u8]) -> Option<&'a Object> {
    let mut current = field;
    for _ in 0..32 {
        if let Ok(value) = current.get(key) {
            return Some(pdf.resolve(value));
        }
        let parent_id = current.get(b"Parent").ok()?.as_reference().ok()?;
        current = pdf.doc.get_dictionary(parent_id).ok()?;
    }
    None
}

/// Every terminal field in the form, in document order.
pub fn list_fields(pdf: &Pdf) -> Vec<FieldInfo> {
    let Some(form) = acroform(pdf) else { return Vec::new() };
    let Ok(fields) = form.get(b"Fields") else { return Vec::new() };
    let page_numbers = page_numbers_by_id(pdf);
    let mut found = Vec::new();
    let mut visited = HashSet::new();
    for field in pdf.resolve(fields).as_array().cloned().unwrap_or_default() {
        if let Ok(id) = field.as_reference() {
            collect_field(pdf, id, "", &page_numbers, &mut visited, &mut found);
        }
    }
    found
}

fn collect_field(pdf: &Pdf, id: ObjectId, parent_name: &str, page_numbers: &BTreeMap<ObjectId, u32>, visited: &mut HashSet<ObjectId>, found: &mut Vec<FieldInfo>) {
    if !visited.insert(id) {
        return;
    }
    let Ok(field) = pdf.doc.get_dictionary(id) else { return };
    let partial = field.get(b"T").ok().and_then(object_to_text);
    let name = match (&partial, parent_name.is_empty()) {
        (Some(part), true) => part.clone(),
        (Some(part), false) => format!("{parent_name}.{part}"),
        (None, _) => parent_name.to_string(),
    };
    let kids: Vec<ObjectId> = field.get(b"Kids").ok().and_then(|k| pdf.resolve(k).as_array().ok()).map(|k| k.iter().filter_map(|o| o.as_reference().ok()).collect()).unwrap_or_default();
    // Kids that have their own names are fields; kids without are widgets.
    let child_fields: Vec<ObjectId> = kids.iter().copied().filter(|kid| pdf.doc.get_dictionary(*kid).map(|d| d.has(b"T")).unwrap_or(false)).collect();
    if !child_fields.is_empty() {
        for kid in child_fields {
            collect_field(pdf, kid, &name, page_numbers, visited, found);
        }
        return;
    }
    let widgets: Vec<ObjectId> = if kids.is_empty() { vec![id] } else { kids };
    let flags = inherited(pdf, field, b"Ff").and_then(|f| f.as_i64().ok()).unwrap_or(0);
    let field_type = match inherited(pdf, field, b"FT").and_then(|t| t.as_name().ok()) {
        Some(b"Tx") => FieldType::Text,
        Some(b"Btn") if flags & FLAG_PUSH_BUTTON != 0 => FieldType::PushButton,
        Some(b"Btn") if flags & FLAG_RADIO != 0 => FieldType::RadioGroup,
        Some(b"Btn") => FieldType::CheckBox,
        Some(b"Ch") if flags & FLAG_COMBO != 0 => FieldType::ComboBox,
        Some(b"Ch") => FieldType::ListBox,
        Some(b"Sig") => FieldType::Signature,
        _ => FieldType::Unknown,
    };
    let value = match inherited(pdf, field, b"V") {
        Some(Object::Array(items)) => items.iter().filter_map(object_to_text).collect::<Vec<_>>().join(", "),
        Some(Object::Dictionary(_)) => "signed".into(),
        Some(other) => object_to_text(other).unwrap_or_default(),
        None => String::new(),
    };
    let mut options: Vec<String> = inherited(pdf, field, b"Opt")
        .and_then(|o| o.as_array().ok())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| match pdf.resolve(item) {
                    Object::Array(pair) => pair.get(1).and_then(object_to_text),
                    other => object_to_text(other),
                })
                .collect()
        })
        .unwrap_or_default();
    if matches!(field_type, FieldType::RadioGroup | FieldType::CheckBox) && options.is_empty() {
        for &widget in &widgets {
            options.extend(on_state_name(pdf, widget));
        }
        options.dedup();
    }
    let widget_places = widgets
        .iter()
        .filter_map(|&widget| {
            let dict = pdf.doc.get_dictionary(widget).ok()?;
            let rect = rectangle_from_object(&pdf.doc, dict.get(b"Rect").ok()?).ok()?;
            let page = dict.get(b"P").ok().and_then(|p| p.as_reference().ok()).and_then(|p| page_numbers.get(&p).copied()).or_else(|| page_of_annotation(pdf, widget))?;
            Some((page, rect))
        })
        .collect();
    found.push(FieldInfo {
        name,
        field_type,
        value,
        options,
        required: flags & FLAG_REQUIRED != 0,
        read_only: flags & FLAG_READ_ONLY != 0,
        multiline: flags & FLAG_MULTILINE != 0,
        max_length: inherited(pdf, field, b"MaxLen").and_then(|m| m.as_i64().ok()).map(|m| m as u32),
        tooltip: field.get(b"TU").ok().and_then(object_to_text).unwrap_or_default(),
        widgets: widget_places,
        id: Some(id),
    });
}

fn page_of_annotation(pdf: &Pdf, annotation_id: ObjectId) -> Option<u32> {
    for (index, page_id) in pdf.page_ids().into_iter().enumerate() {
        let page = pdf.doc.get_dictionary(page_id).ok()?;
        if let Ok(annots) = page.get(b"Annots") {
            if pdf.resolve(annots).as_array().ok()?.iter().any(|a| a.as_reference().ok() == Some(annotation_id)) {
                return Some(index as u32 + 1);
            }
        }
    }
    None
}

/// The name of a checkbox or radio button's "on" appearance (anything but Off).
fn on_state_name(pdf: &Pdf, widget_id: ObjectId) -> Option<String> {
    let widget = pdf.doc.get_dictionary(widget_id).ok()?;
    let normal = pdf.resolve(pdf.resolve(widget.get(b"AP").ok()?).as_dict().ok()?.get(b"N").ok()?).as_dict().ok()?;
    normal.iter().map(|(key, _)| String::from_utf8_lossy(key).into_owned()).find(|key| key != "Off")
}

/// Fills fields by name. Checkboxes take `true`/`yes`/`on`/`1` or their
/// on-state name; radio groups take the name of the button to select.
/// Returns the names that matched no field.
pub fn fill_fields(pdf: &mut Pdf, values: &BTreeMap<String, String>) -> Result<Vec<String>> {
    let fields = list_fields(pdf);
    let mut unmatched = Vec::new();
    for (name, value) in values {
        let Some(field) = fields.iter().find(|f| &f.name == name) else {
            unmatched.push(name.clone());
            continue;
        };
        let Some(field_id) = field.id else { continue };
        match field.field_type {
            FieldType::Text | FieldType::ComboBox | FieldType::ListBox => {
                pdf.doc.get_dictionary_mut(field_id)?.set("V", text_string(value));
                regenerate_text_appearance(pdf, field_id, value)?;
            }
            FieldType::CheckBox | FieldType::RadioGroup => {
                let wanted_on = matches!(value.to_ascii_lowercase().as_str(), "true" | "yes" | "on" | "1" | "x" | "checked");
                let widgets = widget_ids(pdf, field_id);
                let mut chosen: Option<String> = None;
                for widget in &widgets {
                    let on_name = on_state_name(pdf, *widget).unwrap_or_else(|| "Yes".into());
                    let select = if field.field_type == FieldType::CheckBox { wanted_on || &on_name == value } else { &on_name == value };
                    if select {
                        chosen = Some(on_name.clone());
                    }
                    pdf.doc.get_dictionary_mut(*widget)?.set("AS", Object::Name(if select { on_name.into_bytes() } else { b"Off".to_vec() }));
                }
                pdf.doc.get_dictionary_mut(field_id)?.set("V", Object::Name(chosen.unwrap_or_else(|| "Off".into()).into_bytes()));
            }
            FieldType::PushButton | FieldType::Signature | FieldType::Unknown => unmatched.push(name.clone()),
        }
    }
    Ok(unmatched)
}

fn widget_ids(pdf: &Pdf, field_id: ObjectId) -> Vec<ObjectId> {
    let Ok(field) = pdf.doc.get_dictionary(field_id) else { return Vec::new() };
    match field.get(b"Kids").ok().and_then(|k| pdf.resolve(k).as_array().ok()) {
        Some(kids) => kids.iter().filter_map(|k| k.as_reference().ok()).collect(),
        None => vec![field_id],
    }
}

/// Parses `/Helv 12 Tf 0 g` into a font size and fill color.
fn parse_default_appearance(da: &str) -> (f32, [f32; 3]) {
    let tokens: Vec<&str> = da.split_whitespace().collect();
    let mut size = 0.0;
    let mut color = [0.0; 3];
    for (index, token) in tokens.iter().enumerate() {
        match *token {
            "Tf" if index >= 1 => size = tokens[index - 1].parse().unwrap_or(0.0),
            "g" if index >= 1 => {
                let gray = tokens[index - 1].parse().unwrap_or(0.0);
                color = [gray; 3];
            }
            "rg" if index >= 3 => {
                for channel in 0..3 {
                    color[channel] = tokens[index - 3 + channel].parse().unwrap_or(0.0);
                }
            }
            _ => {}
        }
    }
    (size, color)
}

/// Draws a text field's value into fresh appearance streams for each widget.
fn regenerate_text_appearance(pdf: &mut Pdf, field_id: ObjectId, value: &str) -> Result<()> {
    let field = pdf.doc.get_dictionary(field_id)?.clone();
    let da = inherited(pdf, &field, b"DA").and_then(object_to_text).or_else(|| acroform(pdf).and_then(|f| f.get(b"DA").ok().and_then(object_to_text))).unwrap_or_else(|| "/Helv 0 Tf 0 g".into());
    let flags = inherited(pdf, &field, b"Ff").and_then(|f| f.as_i64().ok()).unwrap_or(0);
    let alignment = inherited(pdf, &field, b"Q").and_then(|q| q.as_i64().ok()).unwrap_or(0);
    let comb_length = if flags & (1 << 24) != 0 { inherited(pdf, &field, b"MaxLen").and_then(|m| m.as_i64().ok()) } else { None };
    let (requested_size, color) = parse_default_appearance(&da);
    // Keep the form's default font resource, which readers use when editing.
    ensure_form_font(pdf)?;
    for widget in widget_ids(pdf, field_id) {
        let rect = rectangle_from_object(&pdf.doc, pdf.doc.get_dictionary(widget)?.get(b"Rect")?)?;
        let (width, height) = (rect[2] - rect[0], rect[3] - rect[1]);
        let multiline = flags & FLAG_MULTILINE != 0;
        let size = if requested_size > 0.0 { requested_size } else if multiline { 10.0 } else { (height * 0.66).clamp(4.0, 14.0) };
        let mut setter = TextSetter::for_texts(StandardFont::Helvetica, [value]);
        let mut content = format!("/Tx BMC q 1 1 {} {} re W n BT {} {} {} rg ", number(width - 2.0), number(height - 2.0), number(color[0]), number(color[1]), number(color[2]));
        let lines: Vec<String> = if multiline { crate::fonts::wrap_text_by(value, width - 4.0, |piece| setter.width(piece, size)) } else { vec![value.replace('\n', " ")] };
        if let Some(cells) = comb_length.filter(|c| *c > 0) {
            let cell = width / cells as f32;
            for (index, character) in value.chars().take(cells as usize).enumerate() {
                let text = character.to_string();
                let x = cell * index as f32 + (cell - setter.width(&text, size)) / 2.0;
                content.push_str(&format!("1 0 0 1 {} {} Tm ", number(x), number((height - size * 0.72) / 2.0)));
                content.push_str(&setter.show(&mut pdf.doc, &text, size));
            }
        } else {
            let mut y = if multiline { height - 2.0 - size } else { (height - size * 0.72) / 2.0 };
            for line in lines {
                let line_width = setter.width(&line, size);
                let x = match alignment {
                    1 => (width - line_width) / 2.0,
                    2 => width - 2.0 - line_width,
                    _ => 2.0,
                };
                content.push_str(&format!("1 0 0 1 {} {} Tm ", number(x), number(y)));
                content.push_str(&setter.show(&mut pdf.doc, &line, size));
                y -= size * 1.15;
            }
        }
        content.push_str("ET Q EMC");
        let fonts = setter.finish(&mut pdf.doc)?;
        let stream = Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => "Form",
                "BBox" => rectangle_to_object([0.0, 0.0, width, height]),
                "Resources" => dictionary! { "Font" => fonts },
            },
            content.into_bytes(),
        );
        let appearance_id = pdf.doc.add_object(stream);
        pdf.doc.get_dictionary_mut(widget)?.set("AP", dictionary! { "N" => appearance_id });
    }
    Ok(())
}

/// Makes sure the form has a default font resource named Helv; returns its id.
fn ensure_form_font(pdf: &mut Pdf) -> Result<ObjectId> {
    let catalog_id = pdf.catalog_id()?;
    let mut form = acroform(pdf).unwrap_or_else(|| dictionary! { "Fields" => Vec::<Object>::new() });
    let mut resources = form.get(b"DR").ok().and_then(|r| pdf.resolve(r).as_dict().ok().cloned()).unwrap_or_default();
    let mut fonts = resources.get(b"Font").ok().and_then(|f| pdf.resolve(f).as_dict().ok().cloned()).unwrap_or_default();
    let font_id = match fonts.get(b"Helv").ok().and_then(|f| f.as_reference().ok()) {
        Some(id) => id,
        None => {
            let id = StandardFont::Helvetica.add_to(&mut pdf.doc);
            fonts.set("Helv", id);
            id
        }
    };
    resources.set("Font", fonts);
    form.set("DR", resources);
    if !form.has(b"DA") {
        form.set("DA", text_string("/Helv 0 Tf 0 g"));
    }
    pdf.doc.get_dictionary_mut(catalog_id)?.set("AcroForm", form);
    Ok(font_id)
}

/// A field to add to the form.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NewField {
    pub name: String,
    pub page: u32,
    pub rect: [f32; 4],
    pub kind: NewFieldKind,
    #[serde(default)]
    pub tooltip: String,
    #[serde(default)]
    pub required: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum NewFieldKind {
    Text { multiline: bool, font_size: f32, max_length: Option<u32>, default_value: String },
    CheckBox { checked: bool },
    /// A group of buttons; each entry is (on-state name, page, rect).
    RadioGroup { buttons: Vec<(String, u32, [f32; 4])>, selected: Option<String> },
    ComboBox { options: Vec<String>, editable: bool },
    ListBox { options: Vec<String> },
    Signature,
}

/// Adds a field with a visible widget and returns the field's id.
pub fn add_field(pdf: &mut Pdf, field: &NewField) -> Result<ObjectId> {
    if list_fields(pdf).iter().any(|existing| existing.name == field.name) {
        bail!("a field named \"{}\" already exists", field.name);
    }
    // Every form needs its default font resource before fields can refer to it.
    ensure_form_font(pdf)?;
    let page_id = pdf.page_id(field.page)?;
    let border = dictionary! { "W" => 1, "S" => "S" };
    let characteristics = dictionary! { "BC" => vec![Object::Real(0.45), Object::Real(0.45), Object::Real(0.45)], "BG" => vec![Object::Real(0.93), Object::Real(0.95), Object::Real(1.0)] };
    let mut flags = if field.required { FLAG_REQUIRED } else { 0 };
    let mut dict = dictionary! {
        "Type" => "Annot",
        "Subtype" => "Widget",
        "T" => text_string(&field.name),
        "Rect" => rectangle_to_object(field.rect),
        "P" => page_id,
        "F" => 4,
        "BS" => border.clone(),
        "MK" => characteristics.clone(),
    };
    if !field.tooltip.is_empty() {
        dict.set("TU", text_string(&field.tooltip));
    }
    let mut widgets_to_place = Vec::new();
    match &field.kind {
        NewFieldKind::Text { multiline, font_size, max_length, default_value } => {
            dict.set("FT", "Tx");
            dict.set("DA", text_string(&format!("/Helv {} Tf 0 g", number(*font_size))));
            if *multiline {
                flags |= FLAG_MULTILINE;
            }
            if let Some(max) = max_length {
                dict.set("MaxLen", *max as i64);
            }
            if !default_value.is_empty() {
                dict.set("V", text_string(default_value));
            }
        }
        NewFieldKind::CheckBox { checked } => {
            dict.set("FT", "Btn");
            dict.set("MK", dictionary! { "BC" => vec![Object::Real(0.45); 3], "CA" => text_string("4") });
            let (on, off) = check_appearances(pdf, field.rect, "check");
            dict.set("AP", dictionary! { "N" => dictionary! { "Yes" => on, "Off" => off } });
            dict.set("V", Object::Name(if *checked { b"Yes".to_vec() } else { b"Off".to_vec() }));
            dict.set("AS", Object::Name(if *checked { b"Yes".to_vec() } else { b"Off".to_vec() }));
        }
        NewFieldKind::RadioGroup { buttons, selected } => {
            flags |= FLAG_RADIO | (1 << 14);
            // The group is a non-widget parent; each button is a widget kid.
            dict = dictionary! { "FT" => "Btn", "T" => text_string(&field.name) };
            if !field.tooltip.is_empty() {
                dict.set("TU", text_string(&field.tooltip));
            }
            dict.set("V", Object::Name(selected.clone().unwrap_or_else(|| "Off".into()).into_bytes()));
            for (state, page, rect) in buttons {
                let (on, off) = check_appearances(pdf, *rect, "circle");
                let is_selected = selected.as_deref() == Some(state.as_str());
                let button_page = pdf.page_id(*page)?;
                widgets_to_place.push((
                    button_page,
                    dictionary! {
                        "Type" => "Annot",
                        "Subtype" => "Widget",
                        "Rect" => rectangle_to_object(*rect),
                        "P" => button_page,
                        "F" => 4,
                        "MK" => dictionary! { "BC" => vec![Object::Real(0.45); 3], "CA" => text_string("l") },
                        "AP" => dictionary! { "N" => dictionary! { state.as_str() => on, "Off" => off } },
                        "AS" => Object::Name(if is_selected { state.clone().into_bytes() } else { b"Off".to_vec() }),
                    },
                ));
            }
        }
        NewFieldKind::ComboBox { options, .. } | NewFieldKind::ListBox { options } => {
            dict.set("FT", "Ch");
            dict.set("DA", text_string("/Helv 0 Tf 0 g"));
            dict.set("Opt", Object::Array(options.iter().map(|option| text_string(option)).collect()));
            if let NewFieldKind::ComboBox { editable, .. } = &field.kind {
                flags |= FLAG_COMBO;
                if *editable {
                    flags |= 1 << 18;
                }
            }
        }
        NewFieldKind::Signature => {
            dict.set("FT", "Sig");
        }
    }
    if flags != 0 {
        dict.set("Ff", flags);
    }
    let field_id = pdf.doc.add_object(dict);
    if widgets_to_place.is_empty() {
        let mut annots = annotation_list(pdf, page_id);
        annots.push(Object::Reference(field_id));
        pdf.doc.get_dictionary_mut(page_id)?.set("Annots", annots);
    } else {
        let mut kids = Vec::new();
        for (button_page, mut widget) in widgets_to_place {
            widget.set("Parent", field_id);
            let widget_id = pdf.doc.add_object(widget);
            kids.push(Object::Reference(widget_id));
            let mut annots = annotation_list(pdf, button_page);
            annots.push(Object::Reference(widget_id));
            pdf.doc.get_dictionary_mut(button_page)?.set("Annots", annots);
        }
        pdf.doc.get_dictionary_mut(field_id)?.set("Kids", kids);
    }
    let catalog_id = pdf.catalog_id()?;
    let mut form = acroform(pdf).unwrap_or_default();
    let mut fields = form.get(b"Fields").ok().and_then(|f| pdf.resolve(f).as_array().ok().cloned()).unwrap_or_default();
    fields.push(Object::Reference(field_id));
    form.set("Fields", fields);
    if matches!(field.kind, NewFieldKind::Signature) {
        form.set("SigFlags", 1);
    }
    pdf.doc.get_dictionary_mut(catalog_id)?.set("AcroForm", form);
    if let NewFieldKind::Text { default_value, .. } = &field.kind {
        regenerate_text_appearance(pdf, field_id, default_value)?;
    }
    Ok(field_id)
}

fn annotation_list(pdf: &Pdf, page_id: ObjectId) -> Vec<Object> {
    pdf.doc.get_dictionary(page_id).ok().and_then(|p| p.get(b"Annots").ok()).and_then(|a| pdf.resolve(a).as_array().ok().cloned()).unwrap_or_default()
}

/// On and off appearances for a checkbox (`check`) or radio button (`circle`).
fn check_appearances(pdf: &mut Pdf, rect: [f32; 4], style: &str) -> (ObjectId, ObjectId) {
    let (width, height) = (rect[2] - rect[0], rect[3] - rect[1]);
    let frame = if style == "circle" {
        let r = width.min(height) / 2.0 - 0.5;
        format!("0.45 G 1 w {}", circle(width / 2.0, height / 2.0, r))
    } else {
        format!("0.45 G 1 w 0.5 0.5 {} {} re S", number(width - 1.0), number(height - 1.0))
    };
    let mark = if style == "circle" {
        let r = width.min(height) / 4.0;
        format!("0 g {}", circle(width / 2.0, height / 2.0, r).replace(" S", " f"))
    } else {
        format!(
            "0 G {} w 1 J 1 j {} {} m {} {} l {} {} l S",
            number((width / 8.0).max(1.0)),
            number(width * 0.2), number(height * 0.5),
            number(width * 0.42), number(height * 0.25),
            number(width * 0.8), number(height * 0.78)
        )
    };
    let bbox = rectangle_to_object([0.0, 0.0, width, height]);
    let on = pdf.doc.add_object(Stream::new(dictionary! { "Type" => "XObject", "Subtype" => "Form", "BBox" => bbox.clone(), "Resources" => Dictionary::new() }, format!("{frame} {mark}").into_bytes()));
    let off = pdf.doc.add_object(Stream::new(dictionary! { "Type" => "XObject", "Subtype" => "Form", "BBox" => bbox, "Resources" => Dictionary::new() }, frame.into_bytes()));
    (on, off)
}

fn circle(cx: f32, cy: f32, r: f32) -> String {
    let k = r * 0.552_284_8;
    format!(
        "{} {} m {} {} {} {} {} {} c {} {} {} {} {} {} c {} {} {} {} {} {} c {} {} {} {} {} {} c S",
        number(cx + r), number(cy),
        number(cx + r), number(cy + k), number(cx + k), number(cy + r), number(cx), number(cy + r),
        number(cx - k), number(cy + r), number(cx - r), number(cy + k), number(cx - r), number(cy),
        number(cx - r), number(cy - k), number(cx - k), number(cy - r), number(cx), number(cy - r),
        number(cx + k), number(cy - r), number(cx + r), number(cy - k), number(cx + r), number(cy),
    )
}

/// Field values as a name → value map, for JSON export.
pub fn export_values(pdf: &Pdf) -> BTreeMap<String, String> {
    list_fields(pdf).into_iter().filter(|f| !matches!(f.field_type, FieldType::PushButton | FieldType::Signature)).map(|f| (f.name, f.value)).collect()
}

/// Field values as XFDF, the XML exchange format Acrobat reads.
pub fn export_xfdf(pdf: &Pdf, source_file_name: &str) -> String {
    let escape = |text: &str| text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;");
    let mut xml = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<xfdf xmlns=\"http://ns.adobe.com/xfdf/\" xml:space=\"preserve\">\n<fields>\n");
    for (name, value) in export_values(pdf) {
        xml.push_str(&format!("  <field name=\"{}\"><value>{}</value></field>\n", escape(&name), escape(&value)));
    }
    xml.push_str(&format!("</fields>\n<f href=\"{}\"/>\n</xfdf>\n", escape(source_file_name)));
    xml
}

/// Reads XFDF field values (flat names) into a map for [`fill_fields`].
pub fn parse_xfdf(xml: &str) -> BTreeMap<String, String> {
    let pattern = regex::Regex::new(r#"(?s)<field\s+name="([^"]*)"\s*>\s*<value>(.*?)</value>"#).expect("valid pattern");
    let unescape = |text: &str| text.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&amp;", "&");
    pattern.captures_iter(xml).map(|c| (unescape(&c[1]), unescape(&c[2]))).collect()
}

/// Field values as CSV rows: name, type, value.
pub fn export_csv(pdf: &Pdf) -> String {
    let quote = |text: &str| format!("\"{}\"", text.replace('"', "\"\""));
    let mut csv = String::from("name,type,value\n");
    for field in list_fields(pdf) {
        csv.push_str(&format!("{},{:?},{}\n", quote(&field.name), field.field_type, quote(&field.value)));
    }
    csv
}

/// Clears every field back to empty or unchecked.
pub fn reset_form(pdf: &mut Pdf) -> Result<()> {
    let mut values = BTreeMap::new();
    for field in list_fields(pdf) {
        let empty = match field.field_type {
            FieldType::CheckBox | FieldType::RadioGroup => "Off",
            FieldType::Text | FieldType::ComboBox | FieldType::ListBox => "",
            _ => continue,
        };
        values.insert(field.name, empty.to_string());
    }
    fill_fields(pdf, &values)?;
    Ok(())
}

/// Finds places on the pages that look like blanks to fill in: runs of
/// underscores become text fields and ballot boxes (☐ □) become checkboxes.
/// Returns the proposed fields without adding them.
pub fn detect_fields(document: &pdfium_render::prelude::PdfDocument) -> Result<Vec<NewField>> {
    let mut proposals = Vec::new();
    let mut counter = 1;
    for page_index in 0..document.pages().len() as usize {
        let text = crate::engine::page_text(document, page_index)?;
        let underscores = regex::Regex::new(r"_{4,}").expect("valid pattern");
        let label_before = |end_char: usize| -> String {
            let (string, offsets) = text.as_string();
            let position = offsets.iter().position(|&o| o >= end_char).unwrap_or(0);
            let before: String = string.chars().take(position).collect();
            let line = before.rsplit('\n').next().unwrap_or("");
            line.trim().trim_end_matches(':').trim_end_matches('_').trim().to_string()
        };
        for found in text.find(&underscores) {
            for rect in &found.rects {
                let height = (rect[3] - rect[1]).max(10.0);
                let first_char = text.chars.iter().position(|c| c.rect == *rect || (c.rect[0] - rect[0]).abs() < 0.5 && (c.rect[1] - rect[1]).abs() < 2.0).unwrap_or(0);
                let label = label_before(first_char);
                let name = if label.is_empty() { format!("field{counter}") } else { crate::pages::safe_file_stem(&label).to_lowercase() };
                counter += 1;
                proposals.push(NewField {
                    name,
                    page: page_index as u32 + 1,
                    rect: [rect[0], rect[1], rect[2], rect[1] + height * 1.1],
                    kind: NewFieldKind::Text { multiline: false, font_size: 0.0, max_length: None, default_value: String::new() },
                    tooltip: label,
                    required: false,
                });
            }
        }
        for item in text.chars.iter().filter(|c| matches!(c.character, '☐' | '□' | '❏' | '▢')) {
            proposals.push(NewField {
                name: format!("check{counter}"),
                page: page_index as u32 + 1,
                rect: item.rect,
                kind: NewFieldKind::CheckBox { checked: false },
                tooltip: String::new(),
                required: false,
            });
            counter += 1;
        }
    }
    // Field names must be unique.
    let mut seen = HashSet::new();
    for proposal in &mut proposals {
        let base = proposal.name.clone();
        let mut suffix = 2;
        while !seen.insert(proposal.name.clone()) {
            proposal.name = format!("{base}_{suffix}");
            suffix += 1;
        }
    }
    Ok(proposals)
}

/// Draws every visible annotation's appearance into its page's content and
/// removes the annotations, so the result can no longer be edited. With
/// `forms_only`, only form fields are flattened and comments remain.
pub fn flatten(pdf: &mut Pdf, forms_only: bool) -> Result<usize> {
    let mut flattened = 0;
    for page_id in pdf.page_ids() {
        let annots = annotation_list(pdf, page_id);
        if annots.is_empty() {
            continue;
        }
        let mut kept = Vec::new();
        let mut content = String::new();
        for entry in annots {
            let Ok(annotation_id) = entry.as_reference() else { continue };
            let Ok(annotation) = pdf.doc.get_dictionary(annotation_id).cloned() else { continue };
            let subtype = annotation.get(b"Subtype").ok().and_then(|s| s.as_name().ok()).map(|s| s.to_vec()).unwrap_or_default();
            let is_widget = subtype == b"Widget";
            if subtype == b"Popup" {
                // Pop-ups vanish with their parents; keep them only if the parent stays.
                if forms_only {
                    kept.push(entry);
                }
                continue;
            }
            if subtype == b"Link" || (forms_only && !is_widget) {
                kept.push(entry);
                continue;
            }
            let flags = annotation.get(b"F").and_then(|f| f.as_i64()).unwrap_or(0);
            let hidden = flags & 2 != 0 || flags & 32 != 0;
            if let (false, Some(appearance_id)) = (hidden, normal_appearance(pdf, &annotation)) {
                let rect = rectangle_from_object(&pdf.doc, annotation.get(b"Rect")?)?;
                let appearance = pdf.doc.get_object(appearance_id)?.as_stream()?.clone();
                let bbox = appearance.dict.get(b"BBox").ok().and_then(|b| rectangle_from_object(&pdf.doc, b).ok()).unwrap_or(rect);
                let matrix: Vec<f32> = appearance.dict.get(b"Matrix").ok().and_then(|m| pdf.resolve(m).as_array().ok()).map(|m| m.iter().filter_map(|v| v.as_float().ok()).collect()).filter(|m: &Vec<f32>| m.len() == 6).unwrap_or_else(|| vec![1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
                // Transform the box by the form matrix, then map it onto Rect (PDF 32000-1, 12.5.5).
                let corners = [(bbox[0], bbox[1]), (bbox[2], bbox[1]), (bbox[0], bbox[3]), (bbox[2], bbox[3])].map(|(x, y)| (matrix[0] * x + matrix[2] * y + matrix[4], matrix[1] * x + matrix[3] * y + matrix[5]));
                let min_x = corners.iter().map(|c| c.0).fold(f32::MAX, f32::min);
                let max_x = corners.iter().map(|c| c.0).fold(f32::MIN, f32::max);
                let min_y = corners.iter().map(|c| c.1).fold(f32::MAX, f32::min);
                let max_y = corners.iter().map(|c| c.1).fold(f32::MIN, f32::max);
                let scale_x = if max_x > min_x { (rect[2] - rect[0]) / (max_x - min_x) } else { 1.0 };
                let scale_y = if max_y > min_y { (rect[3] - rect[1]) / (max_y - min_y) } else { 1.0 };
                let name = pdf.add_page_resource(page_id, b"XObject", "VlFl", Object::Reference(appearance_id))?;
                // The appearance stream needs the form type to be used with Do.
                if let Ok(stream) = pdf.doc.get_object_mut(appearance_id).and_then(|o| o.as_stream_mut()) {
                    stream.dict.set("Subtype", "Form");
                    stream.dict.set("Type", "XObject");
                }
                content.push_str(&format!(
                    "q {} 0 0 {} {} {} cm /{name} Do Q\n",
                    number(scale_x),
                    number(scale_y),
                    number(rect[0] - min_x * scale_x),
                    number(rect[1] - min_y * scale_y)
                ));
            }
            flattened += 1;
        }
        if !content.is_empty() {
            pdf.add_page_content(page_id, content.into_bytes(), true)?;
        }
        pdf.doc.get_dictionary_mut(page_id)?.set("Annots", kept);
    }
    if !forms_only || flattened > 0 {
        let catalog_id = pdf.catalog_id()?;
        let remaining_widgets = pdf.page_ids().iter().any(|&page| {
            annotation_list(pdf, page).iter().any(|a| a.as_reference().ok().and_then(|id| pdf.doc.get_dictionary(id).ok()).and_then(|d| d.get(b"Subtype").ok()).and_then(|s| s.as_name().ok()) == Some(b"Widget"))
        });
        if !remaining_widgets {
            pdf.doc.get_dictionary_mut(catalog_id)?.remove(b"AcroForm");
        }
    }
    Ok(flattened)
}

/// The appearance stream a reader would draw for an annotation right now.
fn normal_appearance(pdf: &Pdf, annotation: &Dictionary) -> Option<ObjectId> {
    let appearances = pdf.resolve(annotation.get(b"AP").ok()?).as_dict().ok()?;
    match appearances.get(b"N").ok()? {
        Object::Reference(id) => match pdf.doc.get_object(*id).ok()? {
            Object::Stream(_) => Some(*id),
            Object::Dictionary(states) => {
                let state = annotation.get(b"AS").ok()?.as_name().ok()?;
                states.get(state).ok()?.as_reference().ok()
            }
            _ => None,
        },
        Object::Dictionary(states) => {
            let state = annotation.get(b"AS").ok()?.as_name().ok()?;
            states.get(state).ok()?.as_reference().ok()
        }
        _ => None,
    }
}

/// Reads a field by name, for scripts and tests.
pub fn field_value(pdf: &Pdf, name: &str) -> Result<String> {
    list_fields(pdf).into_iter().find(|f| f.name == name).map(|f| f.value).ok_or_else(|| anyhow!("no field named \"{name}\""))
}
