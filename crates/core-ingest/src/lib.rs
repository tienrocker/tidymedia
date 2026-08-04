//! core-ingest: organize engine (M5) + import sources (M6).
//!
//! M5: date resolution 4 tầng + template engine cho tên thư mục/file đích.
//! Mọi logic ở đây THUẦN (không fs, không DB) để unit-test được đầy đủ.

pub mod date;
pub mod planner;
pub mod template;
