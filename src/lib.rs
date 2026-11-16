//  ____                 ____
// | __ )  _   _  _   _ |  _ \   ___ __   __
// |  _ \ | | | || | | || | | | / _ \\ \ / /
// | |_) || |_| || |_| || |_| ||  __/ \ V /
// |____/  \__,_| \__,_||____/  \___|  \_/
//
// BTMS · Buu's Tiles Map Scanner
// Author : BuuDevOff · https://github.com/BuuDevOff
// SPDX-License-Identifier: GPL-3.0-only

//! BTMS, Buu's Tiles Map Scanner: the whole map of GTA V exported as web map tiles from the
//! user's own game files, offline and drawn by the graphics card when there is one, with the
//! unprotected map resources of a FiveM server. The game folder is only read.
//!
//! The crate is the program: `main.rs` only calls [`cli::main`]. Its modules:
//!
//! - `cli`: the command line, its global options and the commands on the archives alone;
//! - `game`: the install folder, the keys, and the archives (`.rpf`) as one tree of files;
//! - `formats`: the game's file formats (resources, placements, models, textures, interiors);
//! - `world`: the map as a whole, the server's resources, the water, the holes of the ground;
//! - `render`: the drawing, by the graphics card (`render::gpu`) or the processor, and its light;
//! - `tiles`: the tiles, their formats and how they are written; `update`: a map drawn again in part;
//! - `interface`: the local web interface and its server; `progress`: the lines a job sends it;
//! - `config`: the map configuration; `probe`: everything about one block; `util`: threads, timing.

pub mod cli;
mod config;
mod formats;
mod game;
mod interface;
mod probe;
mod progress;
mod render;
mod tiles;
mod update;
mod util;
mod world;
