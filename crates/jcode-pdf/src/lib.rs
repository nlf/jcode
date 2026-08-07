use anyhow::{Result, anyhow};
use std::panic::AssertUnwindSafe;
use std::path::Path;

/// Extract text from a PDF.
///
/// `pdf_extract` panics on some exotic/malformed PDFs (e.g. "unexpected entry
/// in unicode map"). A panic inside a tool task kills the task and surfaces a
/// raw panic to the model, so recover it into a normal error (see #573).
pub fn extract_text(path: &Path) -> Result<String> {
    let result = std::panic::catch_unwind(AssertUnwindSafe(|| pdf_extract::extract_text(path)));
    match result {
        Ok(Ok(text)) => Ok(text),
        Ok(Err(err)) => Err(err.into()),
        Err(payload) => Err(anyhow!(
            "PDF text extraction failed (parser panic: {})",
            panic_message(&payload)
        )),
    }
}

/// Extract text from a PDF, one entry per page.
///
/// Callers that want page boundaries must use this rather than splitting the
/// output of [`extract_text`]. `pdf_extract`'s `PlainTextOutput::end_page` is
/// a no-op, so the combined text contains no page separator of any kind: a
/// five-page document comes back as one run of text. Code that split on `\x0c`
/// silently saw every document as a single page.
///
/// Panics are recovered for the same reason as in [`extract_text`].
pub fn extract_text_by_page(path: &Path) -> Result<Vec<String>> {
    let result =
        std::panic::catch_unwind(AssertUnwindSafe(|| pdf_extract::extract_text_by_pages(path)));
    match result {
        Ok(Ok(pages)) => Ok(pages),
        Ok(Err(err)) => Err(err.into()),
        Err(payload) => Err(anyhow!(
            "PDF text extraction failed (parser panic: {})",
            panic_message(&payload)
        )),
    }
}

fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn malformed_pdf_returns_error_instead_of_panicking() {
        let dir = std::env::temp_dir().join("jcode-pdf-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("malformed.pdf");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(b"%PDF-1.7\nnot a real pdf body\n%%EOF\n")
            .unwrap();
        drop(f);

        let err = extract_text(&path).err();
        assert!(err.is_some(), "expected an error for a malformed PDF");
        let _ = std::fs::remove_file(&path);
    }

    /// Build a minimal PDF with one distinctive word per page.
    fn multipage_pdf(words: &[&str]) -> Vec<u8> {
        let n = words.len();
        let mut objects: Vec<String> = Vec::new();
        objects.push("<< /Type /Catalog /Pages 2 0 R >>".to_string());
        let kids: Vec<String> = (0..n).map(|i| format!("{} 0 R", 3 + i * 2)).collect();
        objects.push(format!(
            "<< /Type /Pages /Kids [{}] /Count {n} >>",
            kids.join(" ")
        ));
        for (i, word) in words.iter().enumerate() {
            objects.push(format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents {} 0 R \
                 /Resources << /Font << /F1 {} 0 R >> >> >>",
                4 + i * 2,
                3 + n * 2
            ));
            let stream = format!("BT /F1 24 Tf 72 700 Td ({word}) Tj ET");
            objects.push(format!(
                "<< /Length {} >>\nstream\n{stream}\nendstream",
                stream.len()
            ));
        }
        objects.push("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string());

        let mut out = String::from("%PDF-1.4\n");
        let mut offsets = Vec::new();
        for (i, object) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.push_str(&format!("{} 0 obj\n{object}\nendobj\n", i + 1));
        }
        let xref = out.len();
        out.push_str(&format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1));
        for offset in offsets {
            out.push_str(&format!("{offset:010} 00000 n \n"));
        }
        out.push_str(&format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        ));
        out.into_bytes()
    }

    /// Page extraction must return one entry per page.
    ///
    /// Regression for a silent wrong answer: callers used to split the output
    /// of `extract_text` on `\x0c`, but `pdf_extract`'s `PlainTextOutput`
    /// implements `end_page` as a no-op and emits no separator at all. Every
    /// document therefore looked like a single page, so a request for page 3
    /// of 5 reported that the page did not exist.
    #[test]
    fn extraction_by_page_reports_real_page_boundaries() {
        let dir = std::env::temp_dir().join("jcode-pdf-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("multipage.pdf");
        std::fs::write(
            &path,
            multipage_pdf(&["ALPHAONE", "BRAVOTWO", "CHARLIETHREE"]),
        )
        .unwrap();

        let pages = extract_text_by_page(&path).expect("multi-page extraction should succeed");
        assert_eq!(pages.len(), 3, "expected one entry per page, got {pages:?}");
        assert!(pages[0].contains("ALPHAONE"), "{pages:?}");
        assert!(pages[1].contains("BRAVOTWO"), "{pages:?}");
        assert!(pages[2].contains("CHARLIETHREE"), "{pages:?}");

        // Each page must hold only its own text, or a selection would return
        // neighbouring pages too.
        assert!(!pages[0].contains("BRAVOTWO"), "page 1 leaked page 2");

        // The premise the old approach rested on, recorded so a future change
        // back to splitting combined text is caught here.
        let combined = extract_text(&path).expect("combined extraction should succeed");
        assert!(
            !combined.contains('\x0c'),
            "no form feed is emitted; do not split combined text on it"
        );

        let _ = std::fs::remove_file(&path);
    }
}
