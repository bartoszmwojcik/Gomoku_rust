//! The Gomoku web application: Axum routes, Askama pages, middleware.

pub mod api;
pub mod app;
pub mod client_address;
pub mod clock;
pub mod config;
pub mod email;
pub mod health;
pub mod i18n;
pub mod pages;
pub mod password;
pub mod routes_auth;
pub mod routes_email;
pub mod routes_password;
pub mod security;
pub mod server;
pub mod session;
