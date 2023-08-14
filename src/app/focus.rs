#[derive(Clone, Debug, Default, PartialEq)]
pub enum Focus {
    Header,
    Prompt,
    #[default]
    Table,
}
