use super::*;
use anyhow::{Context as _, Result, ensure};
use space::{pdf::Pdf, store::MAX_PDF_CHAT_SIZE};
use std::{fs::OpenOptions, os::unix::fs::OpenOptionsExt, path::PathBuf};

#[derive(Default)]
pub(super) struct AttachmentState {
    pub(super) pdfs: Vec<Pdf>,
    pub(super) loading: Option<Uuid>,
}

impl Workspace {
    pub(super) fn can_attach_pdf(&self) -> bool {
        self.chat.as_ref().is_some_and(|chat| !chat.archived)
            && !self.running()
            && !self.waiting
            && !self.pending_open
            && self.submission.is_none()
            && self.attachments.loading.is_none()
            && self.file.is_none()
            && self.rename.is_none()
            && self.chat_menu.is_none()
            && self.model_menu.is_none()
    }

    pub(super) fn attach_pdf(&mut self, cx: &mut Context<Self>) {
        if !self.can_attach_pdf() {
            return;
        }
        let chat_id = self.chat.as_ref().unwrap().id;
        let request_id = Uuid::new_v4();
        let revision = self.input.read(cx).revision();
        let used: usize = self.attachments.pdfs.iter().map(Pdf::encoded_len).sum();
        let available = MAX_PDF_CHAT_SIZE.saturating_sub(used as u64);
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Attach PDF".into()),
        });
        self.attachments.loading = Some(request_id);
        self.error = None;
        let background = cx.background_executor().clone();
        cx.spawn(async move |view, cx| {
            let result = match paths.await {
                Ok(Ok(Some(paths))) => background
                    .spawn(async move { load_pdfs(paths, available) })
                    .await
                    .map(Some),
                Ok(Ok(None)) => Ok(None),
                Ok(Err(error)) => Err(error),
                Err(error) => Err(error.into()),
            };
            let _ = view.update(cx, |view, cx| {
                view.finish_pdf_load(chat_id, request_id, revision, result, cx);
            });
        })
        .detach();
        cx.notify();
    }

    fn finish_pdf_load(
        &mut self,
        chat_id: Uuid,
        request_id: Uuid,
        revision: u64,
        result: Result<Option<Vec<Pdf>>>,
        cx: &mut Context<Self>,
    ) {
        if self.attachments.loading != Some(request_id) {
            return;
        }
        self.attachments.loading = None;
        if self.chat.as_ref().is_some_and(|chat| chat.id == chat_id)
            && !self.pending_open
            && self.input.read(cx).revision() == revision
        {
            match result {
                Ok(Some(pdfs)) => self.attachments.pdfs.extend(pdfs),
                Ok(None) => {}
                Err(error) => self.error = Some(format!("Cannot attach PDF: {error:#}")),
            }
        }
        cx.notify();
    }

    pub(super) fn pdf_attachments(&self, cx: &Context<Self>) -> Stateful<Div> {
        div()
            .id("pdf-attachments")
            .w_full()
            .h(px(28.))
            .flex_shrink_0()
            .overflow_x_scroll()
            .flex()
            .items_center()
            .gap_2()
            .children(
                self.attachments
                    .pdfs
                    .iter()
                    .enumerate()
                    .map(|(index, pdf)| {
                        div()
                            .flex()
                            .flex_shrink_0()
                            .items_center()
                            .gap_2()
                            .max_w(px(240.))
                            .px_2()
                            .py_1()
                            .rounded_sm()
                            .bg(rgb(0x383842))
                            .text_xs()
                            .child(div().min_w_0().truncate().child(pdf.name().to_owned()))
                            .child(
                                div()
                                    .id(("remove-pdf", index))
                                    .flex_shrink_0()
                                    .cursor_pointer()
                                    .child(Icon::Close.render(12., 0x9797a5))
                                    .when(self.submission.is_some(), |view| {
                                        view.opacity(0.4).cursor_default()
                                    })
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if this.submission.is_none()
                                            && index < this.attachments.pdfs.len()
                                        {
                                            this.attachments.pdfs.remove(index);
                                            this.attachments.loading = None;
                                            cx.notify();
                                        }
                                    })),
                            )
                    }),
            )
    }
}

fn load_pdfs(paths: Vec<PathBuf>, mut available: u64) -> Result<Vec<Pdf>> {
    let mut pdfs = Vec::new();
    for path in paths {
        let pdf = read_pdf(&path).with_context(|| format!("Cannot read {}", path.display()))?;
        ensure!(
            pdf.encoded_len() as u64 <= available,
            "PDF data does not fit in a chat"
        );
        available -= pdf.encoded_len() as u64;
        pdfs.push(pdf);
    }
    Ok(pdfs)
}

fn read_pdf(path: &std::path::Path) -> Result<Pdf> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)?;
    ensure!(file.metadata()?.is_file(), "Select a regular PDF file");
    let name = path
        .file_name()
        .context("Missing PDF filename")?
        .to_string_lossy()
        .into_owned();
    Pdf::read(name, file)
}
