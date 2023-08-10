#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Focus {
    Header,
    Prompt,
    #[default]
    Content,
}

impl Focus {
    pub fn is_prompt(&self) -> bool {
        matches!(self, Focus::Prompt)
    }

    pub fn next(&mut self) {
        if self.is_prompt() {
            return;
        }
        match self {
            Self::Header => *self = Self::Content,
            Self::Content => *self = Self::Header,
            _ => todo!(),
        }
    }

    pub fn previous(&mut self) {
        // `next()` and `previous()` are equivalent when there are only two focussable areas
        self.next()
    }
}
