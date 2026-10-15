use std::io::{self, BufRead, Write};
use vanta_extension_protocol::Request;
use wasmtime::{Config, Engine, Instance, Module, Store, StoreLimits, StoreLimitsBuilder};

struct HostState {
    limits: StoreLimits,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut config = Config::new();
    config.consume_fuel(true);
    config.epoch_interruption(true);
    let engine = Engine::new(&config)?;
    for line in io::stdin().lock().lines() {
        match Request::decode(&line?) {
            Ok(Request::Ping { id }) => println!("1\tok\t{id}\tpong"),
            Ok(Request::Execute { id, module, fuel }) => match execute(&engine, &module, fuel) {
                Ok(value) => println!("1\tok\t{id}\t{value}"),
                Err(error) => println!("1\terror\t{id}\t{error}"),
            },
            Ok(Request::Cancel { id }) => {
                engine.increment_epoch();
                println!("1\tok\t{id}\tcancelled");
            }
            Ok(Request::Shutdown) => break,
            Err(error) => println!("1\terror\t0\t{error}"),
        }
        io::stdout().flush()?;
    }
    Ok(())
}

fn execute(engine: &Engine, path: &str, fuel: u64) -> Result<i32, Box<dyn std::error::Error>> {
    let module = Module::from_file(engine, path)?;
    let state = HostState {
        limits: StoreLimitsBuilder::new()
            .memory_size(64 * 1024 * 1024)
            .instances(1)
            .tables(4)
            .build(),
    };
    let mut store = Store::new(engine, state);
    store.limiter(|state| &mut state.limits);
    store.set_fuel(fuel.min(10_000_000))?;
    store.set_epoch_deadline(1);
    let instance = Instance::new(&mut store, &module, &[])?;
    let run = instance.get_typed_func::<(), i32>(&mut store, "run")?;
    Ok(run.call(&mut store, ())?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn engine() -> Engine {
        let mut config = Config::new();
        config.consume_fuel(true);
        config.epoch_interruption(true);
        Engine::new(&config).unwrap()
    }

    fn module(source: &str) -> String {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("vanta-extension-{id}.wat"));
        fs::write(&path, source).unwrap();
        path.to_string_lossy().into_owned()
    }

    #[test]
    fn executes_sandboxed_entrypoint() {
        let path = module("(module (func (export \"run\") (result i32) i32.const 42))");
        assert_eq!(execute(&engine(), &path, 10_000).unwrap(), 42);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn fuel_interrupts_runaway_guest() {
        let path = module("(module (func (export \"run\") (result i32) (loop br 0) i32.const 0))");
        assert!(execute(&engine(), &path, 100).is_err());
        fs::remove_file(path).unwrap();
    }
}
