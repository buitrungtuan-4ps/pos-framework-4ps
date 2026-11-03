//! Throwaway spike: one CEL evaluator for the edge (native) and the till (wasm32 in Chromium).
//!
//! The environment is the CEL standard library with **no loop macros**: `Env::default().with_stdlib()`
//! registers no macros at all, and only `has()` is added back, since field presence is a guard a flow
//! needs and is not a loop. `all`, `exists`, `exists_one`, `map` and `filter` stay plain calls, which
//! fail to resolve, so a comprehension cannot be written.

use std::collections::HashMap;
use std::sync::Arc;

use cel::common::ast::Expr;
use cel::parser::Macro;
use cel::{Context, Env, Program, Value};

/// The guards measured, typical of a flow's "if".
pub const EXPRESSIONS: [&str; 5] = [
    "order.total >= 500000 && order.channel == 'DINE_IN'",
    "order.channel in ['DINE_IN', 'TAKEAWAY'] && 'cat-drinks' in order.categories",
    "has(order.guest_count) && order.guest_count > 4",
    "!has(order.member_id) || order.total - order.discount >= 100000",
    "size(order.items) >= 3 && order.items[0].qty > 1 && order.table.startsWith('T')",
];

/// The longest source accepted; the plan's length limit.
pub const MAX_SOURCE_LEN: usize = 512;

/// The environment: standard functions, `has()` only.
pub fn env() -> Env {
    let mut env = Env::default().with_stdlib();
    let has = Macro::global("has", 1, |helper, _target, args| {
        let arg = args.remove(0);
        match arg.expr {
            Expr::Select(mut select) => {
                select.test = true;
                Ok(Some(helper.next_expr(Expr::Select(select))))
            }
            _ => Err(helper.new_error(arg.id, "invalid argument to has()")),
        }
    });
    env.add_macro(has).expect("has is not yet declared");
    env
}

pub fn compile(env: &Env, source: &str) -> Result<Program, String> {
    if source.len() > MAX_SOURCE_LEN {
        return Err(format!("longer than {MAX_SOURCE_LEN}"));
    }
    env.compile(source).map_err(|e| e.to_string())
}

/// A fake order, as the till would hand it to a guard. Obviously fake data.
pub fn order() -> Value {
    let item = |qty: i64| -> Value {
        HashMap::from([("qty".to_string(), Value::Int(qty)), ("sku".to_string(), Value::from("demo-sku"))]).into()
    };
    let mut fields: HashMap<String, Value> = HashMap::new();
    fields.insert("total".into(), Value::Int(650_000));
    fields.insert("discount".into(), Value::Int(50_000));
    fields.insert("channel".into(), Value::from("DINE_IN"));
    fields.insert("table".into(), Value::from("T12"));
    fields.insert("guest_count".into(), Value::Int(5));
    fields.insert(
        "categories".into(),
        Value::List(Arc::new(vec![Value::from("cat-pizza"), Value::from("cat-drinks")])),
    );
    fields.insert("items".into(), Value::List(Arc::new(vec![item(2), item(1), item(3)])));
    fields.into()
}

pub fn context(env: Arc<Env>, order: Value) -> Context<'static, 'static> {
    let mut ctx = Context::with_env(env);
    ctx.add_variable_from_value("order", order);
    ctx
}

// ---- wasm32 surface: no imports, JS times the calls. ----

struct State {
    env: Arc<Env>,
    programs: Vec<Program>,
    ctx: Context<'static, 'static>,
}

static mut STATE: Option<State> = None;

#[allow(static_mut_refs)]
fn state() -> &'static mut State {
    unsafe {
        STATE.get_or_insert_with(|| {
            let env = Arc::new(env());
            let programs = EXPRESSIONS.iter().map(|s| compile(&env, s).expect("compiles")).collect();
            let ctx = context(Arc::clone(&env), order());
            State { env, programs, ctx }
        })
    }
}

/// Builds the environment, compiles every guard and the context. Returns how many compiled.
#[no_mangle]
pub extern "C" fn setup() -> u32 {
    state().programs.len() as u32
}

/// Compiles guard `i` `n` times; returns how many compiled.
#[no_mangle]
pub extern "C" fn compile_n(i: u32, n: u32) -> u32 {
    let s = state();
    (0..n).filter(|_| compile(&s.env, EXPRESSIONS[i as usize]).is_ok()).count() as u32
}

/// Evaluates guard `i` `n` times against a context built once; returns how many were true.
#[no_mangle]
pub extern "C" fn eval_n(i: u32, n: u32) -> u32 {
    let s = state();
    let p = &s.programs[i as usize];
    (0..n).filter(|_| p.execute(&s.ctx) == Ok(Value::Bool(true))).count() as u32
}

/// Builds the context and evaluates guard `i`, `n` times: an order that changed before each check.
#[no_mangle]
pub extern "C" fn eval_fresh_n(i: u32, n: u32) -> u32 {
    let s = state();
    let p = &s.programs[i as usize];
    (0..n)
        .filter(|_| {
            let ctx = context(Arc::clone(&s.env), order());
            p.execute(&ctx) == Ok(Value::Bool(true))
        })
        .count() as u32
}
