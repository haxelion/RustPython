///! This example show cases a very simple REPL.
///! While a much better REPL can be found in ../src/shell,
///! This much smaller REPL is still a useful example because it showcases inserting
///! values and functions into the Python runtime's scope, and showcases use
///! of the compilation mode "Single".
use rustpython_compiler as compiler;
use rustpython_vm as vm;
// these are needed for special memory shenanigans to let us share a variable with Python and Rust
use std::cell::Cell;
use std::rc::Rc;
// this needs to be in scope in order to insert things into scope.globals
use vm::pyobject::ItemProtocol;

// This has to be a macro because it uses the py_compile_bytecode macro,
// which compiles python source to optimized bytecode at compile time, so that
// the program you're embedding this into doesn't take longer to start up.
macro_rules! add_python_function {
    ( $scope:ident, $vm:ident, $src:literal $(,)? ) => {{
        // this has to be in scope to turn a PyValue into a PyRef
        // (a PyRef is a special reference that points to something in the VirtualMachine)
        use vm::pyobject::PyValue;

        // you can safely assume that only one module will be created when passing a source literal
        // to py_compile_bytecode. However, it is also possible to pass directories, which may
        // return more modules.
        let (_, vm::bytecode::FrozenModule { code, .. }): (String, _) =
            vm::py_compile_bytecode!(source = $src)
                .into_iter()
                .collect::<Vec<_>>()
                .pop()
                .expect("No modules found in the provided source!");

        // takes the first constant in the file that's a function
        let def = code
            .get_constants()
            .find_map(|c| match c {
                vm::bytecode::Constant::Code { code } => Some(code),
                _ => None,
            })
            .expect("No functions found in the provided module!");

        // inserts the first function found in the module into the provided scope.
        $scope.globals.set_item(
            &def.obj_name,
            $vm.context().new_pyfunction(
                vm::obj::objcode::PyCode::new(*def.clone()).into_ref(&$vm),
                $scope.clone(),
                None,
                None,
            ),
            &$vm,
        )
    }};
}

fn main() -> vm::pyobject::PyResult<()> {
    // you can also use a raw pointer instead of Rc<Cell<_>>, but that requires usage of unsafe.
    // both methods are ways of circumnavigating the fact that Python doesn't respect Rust's borrow
    // checking rules.
    let on: Rc<Cell<bool>> = Rc::new(Cell::new(true));

    let mut input = String::with_capacity(50);
    let stdin = std::io::stdin();

    let vm = vm::VirtualMachine::new(vm::PySettings::default());
    let scope: vm::scope::Scope = vm.new_scope_with_builtins();

    // typing `quit()` is too long, let's make `on(False)` work instead.
    scope.globals.set_item(
        "on",
        vm.context().new_function({
            let on = Rc::clone(&on);
            move |b: bool| on.set(b)
        }),
        &vm,
    )?;

    // let's include a fibonacci function, but let's be lazy and write it in Python
    add_python_function!(
        scope,
        vm,
        // a fun line to test this with is
        // ''.join( l * fib(i) for i, l in enumerate('supercalifragilistic') )
        r#"def fib(n): return n if n <= 1 else fib(n - 1) + fib(n - 2)"#
    )?;

    while on.get() {
        input.clear();
        stdin
            .read_line(&mut input)
            .expect("Failed to read line of input");

        // this line also automatically prints the output
        // (note that this is only the case when compile::Mode::Single is passed to vm.compile)
        match vm
            .compile(
                &input,
                compiler::compile::Mode::Single,
                "<embedded>".to_owned(),
            )
            .map_err(|err| vm.new_syntax_error(&err))
            .and_then(|code_obj| vm.run_code_obj(code_obj, scope.clone()))
        {
            Ok(output) => {
                // store the last value in the "last" variable
                if !vm.is_none(&output) {
                    scope.globals.set_item("last", output, &vm)?;
                }
            }
            Err(e) => {
                vm::exceptions::print_exception(&vm, &e);
            }
        }
    }

    Ok(())
}