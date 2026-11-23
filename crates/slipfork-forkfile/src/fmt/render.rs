//! Indenting line renderer.

pub(super) struct Renderer {
    out: String,
    indent: usize,
}

impl Renderer {
    pub(super) fn new() -> Self {
        Self {
            out: String::new(),
            indent: 0,
        }
    }

    pub(super) fn line(&mut self, text: &str) {
        for _ in 0..self.indent {
            self.out.push_str("  ");
        }
        self.out.push_str(text);
        self.out.push('\n');
    }

    pub(super) fn append(&mut self, text: &str) {
        if self.out.ends_with('\n') {
            self.out.pop();
        }
        self.out.push(' ');
        self.out.push_str(text);
        self.out.push('\n');
    }

    pub(super) fn blank(&mut self) {
        if !self.out.is_empty() && !self.out.ends_with("\n\n") {
            self.out.push('\n');
        }
    }

    pub(super) fn indented(&mut self, body: impl FnOnce(&mut Self)) {
        self.indent += 1;
        body(self);
        self.indent -= 1;
    }

    pub(super) fn finish(mut self) -> String {
        while self.out.ends_with("\n\n") {
            self.out.pop();
        }
        self.out
    }
}
