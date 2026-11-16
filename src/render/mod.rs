//  ____                 ____
// | __ )  _   _  _   _ |  _ \   ___ __   __
// |  _ \ | | | || | | || | | | / _ \\ \ / /
// | |_) || |_| || |_| || |_| ||  __/ \ V /
// |____/  \__,_| \__,_||____/  \___|  \_/
//
// BTMS · Buu's Tiles Map Scanner
// Author : BuuDevOff · https://github.com/BuuDevOff
// SPDX-License-Identifier: GPL-3.0-only

//! The drawing: the entities of a view drawn into a picture seen from above, by the graphics card
//! ([`gpu`]) or by the processor ([`raster`], [`draw`]), and its light ([`shade`]). [`scene`] turns
//! the entities into batches for either half; [`assets`] and [`shared`] load the models and
//! textures they need.

pub mod assets;
pub mod draw;
pub mod gpu;
pub mod raster;
pub mod scene;
pub mod shade;
pub mod shared;
