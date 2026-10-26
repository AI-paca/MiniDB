// Измеряем только поток теста и только SELECT, без создания исходных строк/БД.
use minidb::layout::Layout;
use minidb::record;
use minidb::{Column, DataType, Database, Expr, Result, Schema, Value};
use std::alloc::{GlobalAlloc, Layout as AllocLayout, System};
use std::cell::Cell;
use std::fs;

thread_local! {
    static COUNT: Cell<Option<usize>> = const { Cell::new(None) };
}
struct Counted;
fn count_allocation() {
    let _ = COUNT.try_with(|count| {
        if let Some(n) = count.get() {
            count.set(Some(n + 1));
        }
    });
}
// SAFETY: все операции делегированы System с исходными указателями и Layout.
unsafe impl GlobalAlloc for Counted {
    unsafe fn alloc(&self, layout: AllocLayout) -> *mut u8 {
        count_allocation();
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: AllocLayout) -> *mut u8 {
        count_allocation();
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: AllocLayout, size: usize) -> *mut u8 {
        count_allocation();
        unsafe { System.realloc(ptr, layout, size) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: AllocLayout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}
#[global_allocator]
static ALLOCATOR: Counted = Counted;

#[test]
fn select_reuses_the_buffer_for_borrowed_strings_without_per_row_allocations() -> Result<()> {
    let root = std::env::temp_dir().join(format!("minidb-projection-alloc-{}", std::process::id()));
    fs::create_dir(&root)?;
    let result = (|| -> Result<()> {
        let schema = Schema::new(
            "items",
            vec![
                Column::new("id", DataType::INT),
                Column::new("name", DataType::STRING),
            ],
            0,
        );
        let layout = Layout::from_schema(&schema)?;
        let mut bytes = Vec::new();
        for id in 1..=1000 {
            let offset = bytes.len();
            bytes.resize(offset + layout.row_size, 0);
            let name = format!("строка {id}");
            record::encode(
                &layout,
                &schema,
                &[Value::Int(id), Value::String(&name)],
                &mut bytes[offset..],
            )?;
        }
        fs::write(root.join("items.schema.sql"), schema.to_text())?;
        fs::write(root.join("items.v1.data"), bytes)?;
        let mut db = Database::open(&root)?;
        let mut sum = 0;
        let mut count = 0;
        COUNT.with(|count| count.set(Some(0)));
        let selected = db.select("items", &["name", "id"], &Expr::All, |values| {
            let Value::String(name) = values[0] else {
                return Err("Нет строки".into());
            };
            assert!(name.starts_with("строка "));
            sum += values[1].int()?;
            count += 1;
            Ok(())
        });
        let allocations = COUNT.with(|count| count.replace(None).unwrap());
        assert_eq!(selected?, 1000);
        assert_eq!(count, 1000);
        assert_eq!(sum, 500500);
        // Индексы проекции, её единственный Vec и BufReader; не 1000 Vec.
        println!("SELECT: {allocations} allocations for {count} rows");
        assert!(allocations <= 4, "SELECT allocated {allocations} times");
        COUNT.with(|count| count.set(Some(0)));
        let updated = db.update("items", &Expr::All, &[("name", Value::String("updated"))]);
        let update_allocations = COUNT.with(|count| count.replace(None).unwrap());
        assert_eq!(updated?, 1000);
        println!("UPDATE: {update_allocations} allocations for 1000 changed rows");
        assert!(
            update_allocations <= 100,
            "UPDATE allocated {update_allocations} times"
        );
        COUNT.with(|count| count.set(Some(0)));
        let noop = db.update("items", &Expr::All, &[("name", Value::String("updated"))]);
        let noop_allocations = COUNT.with(|count| count.replace(None).unwrap());
        assert_eq!(noop?, 0);
        println!("no-op UPDATE: {noop_allocations} allocations for 1000 rows");
        assert!(
            noop_allocations <= 20,
            "no-op UPDATE allocated {noop_allocations} times"
        );
        assert!(db
            .select("items", &["name"], &Expr::All, |_| Err("stop".into()))
            .is_err());
        assert_eq!(db.select("items", &["id"], &Expr::All, |_| Ok(()))?, 1000);
        Ok(())
    })();
    fs::remove_dir_all(root)?;
    result
}
