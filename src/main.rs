use senritsu::{logging, paths};

fn main() {
    let _guard = logging::init(&paths::cache_dir());
}
