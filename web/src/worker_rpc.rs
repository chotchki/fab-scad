//! Shared worker RPC plumbing (wasm): id-matched replies over a PERSISTENT onmessage, plus an
//! onerror that fails every pending call. Exists because the review refuted the one-shot
//! transport twice over: a reply arriving after its call was dropped consumed ANOTHER call's
//! once-closure (crossed replies, wrong model under the picked name), and a 404'd worker
//! script never resolved anything (eternal busy pulse, every button "still working"). Now a
//! stale reply resolves a dead promise harmlessly, and a load failure errors every caller.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

pub struct Rpc {
    pending: Rc<RefCell<HashMap<u32, js_sys::Function>>>,
    next: Cell<u32>,
}

impl Rpc {
    /// Wire persistent onmessage/onerror onto a fresh worker. Replies dispatch by `data.id`.
    pub fn attach(worker: &web_sys::Worker, load_err: &'static str) -> Rc<Rpc> {
        let pending: Rc<RefCell<HashMap<u32, js_sys::Function>>> = Rc::default();

        let p = pending.clone();
        let on_msg =
            Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |e: web_sys::MessageEvent| {
                let data = e.data();
                let id = js_sys::Reflect::get(&data, &JsValue::from_str("id"))
                    .ok()
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0) as u32;
                if let Some(res) = p.borrow_mut().remove(&id) {
                    res.call1(&JsValue::UNDEFINED, &data).ok();
                }
            });
        worker.set_onmessage(Some(on_msg.as_ref().unchecked_ref()));
        on_msg.forget(); // one worker per page load — the leak is a constant

        let p = pending.clone();
        let on_err = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
            let dead: Vec<js_sys::Function> = p.borrow_mut().drain().map(|(_, f)| f).collect();
            for res in dead {
                let o = js_sys::Object::new();
                js_sys::Reflect::set(&o, &JsValue::from_str("ok"), &JsValue::FALSE).ok();
                js_sys::Reflect::set(
                    &o,
                    &JsValue::from_str("error"),
                    &JsValue::from_str(load_err),
                )
                .ok();
                res.call1(&JsValue::UNDEFINED, &o).ok();
            }
        });
        worker.set_onerror(Some(on_err.as_ref().unchecked_ref()));
        on_err.forget();

        Rc::new(Rpc {
            pending,
            next: Cell::new(1),
        })
    }

    /// Reserve a call id + the promise its reply resolves.
    pub fn register(&self) -> (u32, js_sys::Promise) {
        let id = self.next.get();
        self.next.set(id + 1);
        let pending = self.pending.clone();
        let promise = js_sys::Promise::new(&mut |resolve, _| {
            pending.borrow_mut().insert(id, resolve);
        });
        (id, promise)
    }
}
