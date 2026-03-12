use thiserror::Error;

#[derive(Debug, Error)]
pub enum ScanError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Path not allowed by safety checker: {0}")]
    PathBlocked(String),

    #[error("Scanner error: {0}")]
    Scanner(String),
}

#[derive(Debug, Error)]
pub enum ActionError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Path blocked by safety checker: {0}")]
    PathBlocked(String),

    #[error("Command failed: {0}")]
    CommandFailed(String),

    #[error("Action error: {0}")]
    Other(String),
}
