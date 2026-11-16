//  ____                 ____
// | __ )  _   _  _   _ |  _ \   ___ __   __
// |  _ \ | | | || | | || | | | / _ \\ \ / /
// | |_) || |_| || |_| || |_| ||  __/ \ V /
// |____/  \__,_| \__,_||____/  \___|  \_/
//
// BTMS · Buu's Tiles Map Scanner
// Author : BuuDevOff · https://github.com/BuuDevOff
// SPDX-License-Identifier: GPL-3.0-only

//! The game's file formats, read from their bytes: RSC7 resources ([`resource`]), RBF, Meta and
//! PSO documents ([`rbf`], [`meta`], [`pso`]), placements and archetypes ([`ymap`]), interiors
//! ([`mlo`]), models ([`ydr`]) and textures ([`ytd`]).

pub mod meta;
pub mod mlo;
pub mod pso;
pub mod rbf;
pub mod resource;
pub mod ydr;
pub mod ymap;
pub mod ytd;
