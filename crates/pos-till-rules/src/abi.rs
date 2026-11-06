// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The three functions the till calls. Exporting a name is an `unsafe` attribute in Rust 2024, and
//! this is the only place it is used: no pointer is read or written here. The till writes a request
//! into the buffer [`till_rules_input`] reserves, through the module's memory, and reads the answer
//! from where [`till_rules_output`] says. Every length is checked against the buffer it names.
#![expect(
    unsafe_code,
    reason = "exporting the module's three functions by name to the till"
)]

use std::cell::RefCell;

use crate::engine::{Engine, REQUEST_BYTES_MAX};

thread_local! {
    static ENGINE: RefCell<Engine> = RefCell::new(Engine::default());
    static INPUT: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
    static OUTPUT: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

/// Reserves `len` bytes for the next request and returns where they start, or 0 when `len` is
/// over [`REQUEST_BYTES_MAX`]. The buffer is the module's own; the till writes into it.
#[unsafe(no_mangle)]
pub extern "C" fn till_rules_input(len: usize) -> usize {
    if len > REQUEST_BYTES_MAX {
        return 0;
    }
    INPUT.with_borrow_mut(|input| {
        input.clear();
        input.resize(len, 0);
        input.as_ptr() as usize
    })
}

/// Answers the request in the first `len` bytes of the input buffer, and returns the answer's
/// length. A `len` past the buffer is answered as an error, like any other bad request.
#[unsafe(no_mangle)]
pub extern "C" fn till_rules_call(len: usize) -> usize {
    let answer = INPUT.with_borrow(|input| {
        input.get(..len).map_or_else(
            || {
                br#"{"ok":false,"error":"the request is longer than the buffer reserved for it"}"#
                    .to_vec()
            },
            |request| ENGINE.with_borrow_mut(|engine| engine.answer(request)),
        )
    });
    OUTPUT.with_borrow_mut(|output| {
        *output = answer;
        output.len()
    })
}

/// Where the last answer starts; it is as long as [`till_rules_call`] said.
#[unsafe(no_mangle)]
pub extern "C" fn till_rules_output() -> usize {
    OUTPUT.with_borrow(|output| output.as_ptr() as usize)
}

/// Answers `request` as the till's call does, through the same buffers. For tests, natively.
#[must_use]
pub fn call(request: &[u8]) -> Vec<u8> {
    if till_rules_input(request.len()) == 0 && !request.is_empty() {
        return br#"{"ok":false,"error":"the request is too large"}"#.to_vec();
    }
    INPUT.with_borrow_mut(|input| input.copy_from_slice(request));
    let len = till_rules_call(request.len());
    OUTPUT.with_borrow(|output| output.get(..len).map(<[u8]>::to_vec).unwrap_or_default())
}
