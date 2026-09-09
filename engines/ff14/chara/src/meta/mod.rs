//! Binary metadata files Physis does not parse.

pub mod atch;
pub mod cmp;
pub mod eqdp;
pub mod est;
pub mod imc;
pub mod stm;
pub mod tmb;

pub use cmp::CmpFile;
pub use eqdp::EqdpFile;
pub use est::EstFile;
pub use imc::{ImcEntry, ImcFile};
