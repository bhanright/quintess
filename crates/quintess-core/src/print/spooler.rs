//! Printing through the Windows print system.
//!
//! Each page is rendered by PDFium at the printer's resolution (capped at
//! 300 DPI to bound memory; printers smooth the rest) and drawn into the
//! printer's device context, fitted or at actual size, turned to suit the
//! paper. Printer choices such as tray and paper size come from the
//! printer's own properties dialog and are kept as a DEVMODE blob.

use anyhow::{Result, anyhow, bail};
use image::RgbaImage;
use pdfium_render::prelude::PdfDocument;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateDCW, DEVMODEW, DIB_RGB_COLORS, DM_COLOR, DM_COPIES, DM_DUPLEX, DM_IN_BUFFER, DM_IN_PROMPT, DM_OUT_BUFFER, DMCOLOR_COLOR, DMCOLOR_MONOCHROME, DMDUP_HORIZONTAL, DMDUP_SIMPLEX, DMDUP_VERTICAL,
    DeleteDC, GetDeviceCaps, HALFTONE, HDC, HORZRES, LOGPIXELSX, LOGPIXELSY, SRCCOPY, SetBrushOrgEx, SetStretchBltMode, StretchDIBits, VERTRES,
};
use windows::Win32::Graphics::Printing::{ClosePrinter, DocumentPropertiesW, EnumPrintersW, GetDefaultPrinterW, OpenPrinterW, PRINTER_ENUM_CONNECTIONS, PRINTER_ENUM_LOCAL, PRINTER_HANDLE, PRINTER_INFO_4W};
use windows::Win32::Storage::Xps::{AbortDoc, DOCINFOW, EndDoc, EndPage, StartDocW, StartPage};
use windows::core::{PCWSTR, PWSTR};

use super::{Duplex, PaperSize, PrintSettings, PrinterInfo, Scaling};
use crate::engine::{self, RenderOptions};

/// Highest resolution pages are rendered at for printing.
const MAX_PRINT_DPI: f32 = 300.0;

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Printers installed for this user, the default one first.
pub fn printers() -> Result<Vec<PrinterInfo>> {
    let default = default_printer();
    let flags = PRINTER_ENUM_LOCAL | PRINTER_ENUM_CONNECTIONS;
    let mut needed = 0u32;
    let mut count = 0u32;
    // The first call reports the buffer size; it "fails" with that size set.
    let _ = unsafe { EnumPrintersW(flags, PCWSTR::null(), 4, None, &mut needed, &mut count) };
    if needed == 0 {
        return Ok(Vec::new());
    }
    let mut buffer = vec![0u8; needed as usize];
    unsafe { EnumPrintersW(flags, PCWSTR::null(), 4, Some(&mut buffer), &mut needed, &mut count) }.map_err(|e| anyhow!("could not list printers: {e}"))?;
    let entries = unsafe { std::slice::from_raw_parts(buffer.as_ptr() as *const PRINTER_INFO_4W, count as usize) };
    let mut found: Vec<PrinterInfo> = entries
        .iter()
        .filter_map(|entry| unsafe { entry.pPrinterName.to_string().ok() })
        .map(|name| PrinterInfo { is_default: Some(&name) == default.as_ref(), name })
        .collect();
    found.sort_by_key(|p| !p.is_default);
    Ok(found)
}

pub fn default_printer() -> Option<String> {
    let mut length = 0u32;
    unsafe {
        let _ = GetDefaultPrinterW(None, &mut length);
        if length == 0 {
            return None;
        }
        let mut buffer = vec![0u16; length as usize];
        if GetDefaultPrinterW(Some(PWSTR(buffer.as_mut_ptr())), &mut length).as_bool() {
            String::from_utf16(&buffer[..length.saturating_sub(1) as usize]).ok()
        } else {
            None
        }
    }
}

struct OpenPrinter(PRINTER_HANDLE);

impl OpenPrinter {
    fn open(name: &str) -> Result<Self> {
        let name = wide(name);
        let mut handle = PRINTER_HANDLE::default();
        unsafe { OpenPrinterW(PCWSTR(name.as_ptr()), &mut handle, None) }.map_err(|e| anyhow!("could not open the printer: {e}"))?;
        Ok(Self(handle))
    }
}

impl Drop for OpenPrinter {
    fn drop(&mut self) {
        let _ = unsafe { ClosePrinter(self.0) };
    }
}

/// The printer's settings as a DEVMODE blob: `input` merged into the
/// driver's defaults, optionally after showing the driver's own dialog.
fn device_mode(printer: &str, input: Option<&[u8]>, show_dialog: Option<isize>) -> Result<Option<Vec<u8>>> {
    let handle = OpenPrinter::open(printer)?;
    let name = wide(printer);
    let size = unsafe { DocumentPropertiesW(None, handle.0, PCWSTR(name.as_ptr()), None, None, 0) };
    if size <= 0 {
        bail!("the printer driver did not describe its settings");
    }
    let mut output = vec![0u8; size as usize];
    let mut mode = DM_OUT_BUFFER.0;
    if input.is_some() {
        mode |= DM_IN_BUFFER.0;
    }
    if show_dialog.is_some() {
        mode |= DM_IN_PROMPT.0;
    }
    let window = show_dialog.map(|raw| HWND(raw as *mut core::ffi::c_void));
    let result = unsafe {
        DocumentPropertiesW(window, handle.0, PCWSTR(name.as_ptr()), Some(output.as_mut_ptr() as *mut DEVMODEW), input.map(|bytes| bytes.as_ptr() as *const DEVMODEW), mode)
    };
    // IDCANCEL (2) means the person closed the dialog without changes.
    match result {
        2 if show_dialog.is_some() => Ok(None),
        value if value < 0 => bail!("the printer driver rejected the settings"),
        _ => Ok(Some(output)),
    }
}

/// Shows the printer driver's properties dialog (paper, tray, quality…).
/// Returns the updated settings, or `None` if the person canceled.
pub fn printer_properties(printer: &str, parent_window: isize, current: Option<&[u8]>) -> Result<Option<Vec<u8>>> {
    device_mode(printer, current, Some(parent_window))
}

/// Paper sizes are chosen in the driver's properties dialog on Windows.
pub fn paper_sizes(_printer: &str) -> Vec<PaperSize> {
    Vec::new()
}

struct DeviceContext(HDC);

impl Drop for DeviceContext {
    fn drop(&mut self) {
        let _ = unsafe { DeleteDC(self.0) };
    }
}

/// Prints pages of `document`. `progress(done, total)` is called per page.
pub fn print(document: &PdfDocument, title: &str, settings: &PrintSettings, mut progress: impl FnMut(u32, u32)) -> Result<()> {
    if settings.pages.is_empty() {
        bail!("no pages to print");
    }
    let mut mode = device_mode(&settings.printer, settings.device_mode.as_deref(), None)?.ok_or_else(|| anyhow!("no printer settings"))?;
    {
        // Copies are printed by Quintess itself, so the driver prints one of each.
        let devmode = unsafe { &mut *(mode.as_mut_ptr() as *mut DEVMODEW) };
        devmode.Anonymous1.Anonymous1.dmCopies = 1;
        devmode.dmFields |= DM_COPIES;
        if settings.grayscale {
            devmode.dmColor = DMCOLOR_MONOCHROME;
            devmode.dmFields |= DM_COLOR;
        } else if devmode.dmFields.0 & DM_COLOR.0 != 0 {
            devmode.dmColor = DMCOLOR_COLOR;
        }
        let duplex = match settings.duplex {
            Duplex::PrinterDefault => None,
            Duplex::OneSided => Some(DMDUP_SIMPLEX),
            Duplex::LongEdge => Some(DMDUP_VERTICAL),
            Duplex::ShortEdge => Some(DMDUP_HORIZONTAL),
        };
        if let Some(duplex) = duplex {
            devmode.dmDuplex = duplex;
            devmode.dmFields |= DM_DUPLEX;
        }
    }
    let printer_name = wide(&settings.printer);
    let driver = wide("WINSPOOL");
    let dc = unsafe { CreateDCW(PCWSTR(driver.as_ptr()), PCWSTR(printer_name.as_ptr()), PCWSTR::null(), Some(mode.as_ptr() as *const DEVMODEW)) };
    if dc.is_invalid() {
        bail!("could not start printing to {}", settings.printer);
    }
    let dc = DeviceContext(dc);
    let caps = |index| unsafe { GetDeviceCaps(Some(dc.0), index) };
    let (dpi_x, dpi_y) = (caps(LOGPIXELSX).max(72) as f32, caps(LOGPIXELSY).max(72) as f32);
    let (printable_width, printable_height) = (caps(HORZRES).max(1) as f32, caps(VERTRES).max(1) as f32);

    let document_name = wide(title);
    let output = settings.output_file.as_ref().map(|path| wide(&path.to_string_lossy()));
    let info = DOCINFOW {
        cbSize: std::mem::size_of::<DOCINFOW>() as i32,
        lpszDocName: PCWSTR(document_name.as_ptr()),
        lpszOutput: output.as_ref().map(|o| PCWSTR(o.as_ptr())).unwrap_or(PCWSTR::null()),
        lpszDatatype: PCWSTR::null(),
        fwType: 0,
    };
    if unsafe { StartDocW(dc.0, &info) } <= 0 {
        bail!("the printer refused the job");
    }
    let order: Vec<u32> = if settings.collate {
        (0..settings.copies.max(1)).flat_map(|_| settings.pages.iter().copied()).collect()
    } else {
        settings.pages.iter().flat_map(|&page| std::iter::repeat_n(page, settings.copies.max(1) as usize)).collect()
    };
    let total = order.len() as u32;
    for (done, &page_number) in order.iter().enumerate() {
        progress(done as u32 + 1, total);
        if let Err(error) = print_page(&dc, document, page_number, settings, (dpi_x, dpi_y), (printable_width, printable_height)) {
            unsafe {
                AbortDoc(dc.0);
            }
            return Err(error);
        }
    }
    if unsafe { EndDoc(dc.0) } <= 0 {
        bail!("the printer did not accept the end of the job");
    }
    Ok(())
}

fn print_page(dc: &DeviceContext, document: &PdfDocument, page_number: u32, settings: &PrintSettings, (dpi_x, dpi_y): (f32, f32), (printable_width, printable_height): (f32, f32)) -> Result<()> {
    let page = document.pages().get(page_number as i32 - 1)?;
    let (mut page_width, mut page_height) = (page.width().value, page.height().value);
    drop(page);
    let paper_is_landscape = printable_width > printable_height;
    let rotate = settings.auto_rotate && (page_width > page_height) != paper_is_landscape && (page_width - page_height).abs() > 1.0;
    if rotate {
        std::mem::swap(&mut page_width, &mut page_height);
    }
    // Size on paper in device pixels.
    let actual = (page_width / 72.0 * dpi_x, page_height / 72.0 * dpi_y);
    let fit_scale = (printable_width / actual.0).min(printable_height / actual.1);
    let scale = match settings.scaling {
        Scaling::Fit => fit_scale,
        Scaling::ActualSize => 1.0,
        Scaling::ShrinkOversized => fit_scale.min(1.0),
    };
    let (target_width, target_height) = (actual.0 * scale, actual.1 * scale);
    let x = ((printable_width - target_width) / 2.0).round() as i32;
    let y = ((printable_height - target_height) / 2.0).round() as i32;
    // Render no sharper than the paper needs, and never beyond the cap.
    let render_dpi = (dpi_x.min(MAX_PRINT_DPI) * scale).clamp(36.0, MAX_PRINT_DPI);
    let mut image: RgbaImage = engine::render_page(
        document,
        page_number as usize - 1,
        &RenderOptions { dpi: render_dpi, draw_annotations: settings.annotations, draw_form_fields: settings.annotations, grayscale: settings.grayscale },
    )?;
    if rotate {
        // Counterclockwise, as printers conventionally turn landscape pages:
        // the top of the page ends up along the left edge of the paper.
        image = image::imageops::rotate270(&image);
    }
    let (width, height) = image.dimensions();
    // GDI wants blue-green-red-reserved bytes.
    let mut pixels = image.into_raw();
    for pixel in pixels.chunks_exact_mut(4) {
        pixel.swap(0, 2);
        pixel[3] = 0;
    }
    let header = BITMAPINFOHEADER {
        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: width as i32,
        // Negative height: rows run top to bottom.
        biHeight: -(height as i32),
        biPlanes: 1,
        biBitCount: 32,
        biCompression: BI_RGB.0,
        ..Default::default()
    };
    let info = BITMAPINFO { bmiHeader: header, ..Default::default() };
    unsafe {
        if StartPage(dc.0) <= 0 {
            bail!("the printer refused page {page_number}");
        }
        SetStretchBltMode(dc.0, HALFTONE);
        let _ = SetBrushOrgEx(dc.0, 0, 0, None);
        let drawn = StretchDIBits(dc.0, x, y, target_width.round() as i32, target_height.round() as i32, 0, 0, width as i32, height as i32, Some(pixels.as_ptr() as *const core::ffi::c_void), &info, DIB_RGB_COLORS, SRCCOPY);
        if drawn == 0 {
            bail!("drawing page {page_number} for the printer failed");
        }
        if EndPage(dc.0) <= 0 {
            bail!("the printer refused to finish page {page_number}");
        }
    }
    Ok(())
}
