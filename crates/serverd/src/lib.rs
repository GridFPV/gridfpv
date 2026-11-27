//! The Grid remote server (docs/primitives.md §13).
//!
//! A Director runs one event on a laptop at the race. This program runs somewhere that is
//! always on, and holds the events Directors push to it.
//!
//! - [`book`]: the clubs on this server, and the link tokens Directors push with.
//! - [`store`]: the server's copy of each event's log.
//! - [`page`]: the web page the server shows, from a folder or from inside the program.
//! - [`accounts`]: accounts, how they sign in, their roles, and invites.
//! - [`people`]: those over HTTP: who is signed in, invites, a club's members.
//! - [`claims`]: a club's pilots, and which account holds each: how results reach an
//!   account.
//! - [`asks`]: a pilot asking for something only the Director can do, and the Director's
//!   answer.
//! - [`recover`]: an owner helping someone who can no longer sign in.
//! - [`signin`]: signing in with a provider, Discord or Google.
//! - [`push`]: the routes a Director pushes to.
//! - [`read`]: the routes anyone may read: an event, as a visitor may see it.
//! - [`app`]: the one router.
//!
//! **The server never writes an event.** A Director is the only writer of its events
//! (§13.4). What is here only ever adds what a Director sent to the end of a copy.
#![forbid(unsafe_code)]

pub mod accounts;
pub mod app;
pub mod asks;
pub mod book;
pub mod claims;
pub mod join;
pub mod manage;
pub mod page;
pub mod people;
pub mod push;
pub mod read;
pub mod recover;
pub mod signin;
pub mod store;
