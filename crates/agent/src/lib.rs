//! The host agent of TeamTAK.
//!
//! It is the only component that sees the machine and the Docker socket; the web application
//! has neither. It answers the platform, and nobody else, with a read-only report (the
//! machine, this stack's containers, TAK Server's processes, the backups) and runs a closed
//! list of actions: restart one service of this stack, write a backup, hand the Let's
//! Encrypt certificate to TAK Server, tidy TAK Server's logs.
//!
//! It never applies system updates, never reboots, never pulls or upgrades an image, and has
//! no way to run a command or reach a container outside its own Compose project.

pub mod agent;
pub mod api;
pub mod docker;
pub mod host;
pub mod logs;
pub mod parse;
pub mod report;
