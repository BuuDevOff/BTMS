//  ____                 ____
// | __ )  _   _  _   _ |  _ \   ___ __   __
// |  _ \ | | | || | | || | | | / _ \\ \ / /
// | |_) || |_| || |_| || |_| ||  __/ \ V /
// |____/  \__,_| \__,_||____/  \___|  \_/
//
// BTMS · Buu's Tiles Map Scanner
// Author : BuuDevOff · https://github.com/BuuDevOff
// SPDX-License-Identifier: GPL-3.0-only

//! `btms-gui.exe`: BTMS for the Start menu and the desktop. The same program as `btms.exe`, built as
//! a Windows application instead of a console one, so that starting it opens the interface window
//! and nothing else: no terminal pops up. With no arguments it runs the interface, as `btms` does.
//!
//! `btms.exe` stays the command-line program, for terminals and scripts: a Windows application
//! started from a terminal would not hold the prompt nor show what it prints.
//!
//! The interface runs each render as a child process of the executable it runs from, so the
//! renders started from here have no console window either. Their output still reaches the
//! interface through the pipes it opens.

#![windows_subsystem = "windows"]

fn main() {
    btms::cli::main()
}
