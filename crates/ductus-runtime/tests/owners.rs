//! What matching owners to accounts costs (0.6.7.2): what `GET /owners` does per request and the
//! alert judge per notification (read every account, then match every owner), with no former
//! names and with three per account. Run by hand:
//! `cargo test --release -p ductus-runtime --test owners -- --ignored --nocapture`.

use ductus_core::owners;
use ductus_store::testing::TestUsers;
use ductus_store::{Audit, By, NewUser, Role, Store, UserChange};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

const PEOPLE: usize = 300;

/// The median of five runs of `f`.
async fn median<F: AsyncFnMut()>(mut f: F) -> Duration {
    let mut times = Vec::new();
    for _ in 0..5 {
        let start = Instant::now();
        f().await;
        times.push(start.elapsed());
    }
    times.sort();
    times[2]
}

#[tokio::test]
#[ignore]
async fn measure_matching_owners() {
    let Ok(url) = std::env::var("DUCTUS_TEST_DATABASE_URL") else { return };
    let store = Store::connect(&url, 4).await.unwrap();
    let people = TestUsers::new(&url);
    let audit = || Audit::new(By::cli("test"), "test");
    let mut ids = Vec::new();
    for i in 0..PEOPLE {
        let name = people.name("test-user-owners");
        let (display, email) = (format!("Owner {i:03}"), format!("owner{i:03}@example.org"));
        let new = NewUser {
            username: &name,
            display_name: &display,
            email: &email,
            role: Role::Viewer,
            permissions: &[],
            password_hash: Some("x"),
            must_change_password: false,
            source: "local",
        };
        ids.push(store.create_user(&new, &audit()).await.unwrap().id);
    }
    // A third of the owners by email, a third by display name, a third by a name nobody has.
    let texts: Vec<String> = (0..PEOPLE)
        .map(|i| match i % 3 {
            0 => format!("owner{i:03}@example.org"),
            1 => format!("Owner {i:03}"),
            _ => format!("Somebody {i:03}"),
        })
        .collect();
    let teams = BTreeMap::new();

    let report = async |label: &str| {
        let read = median(async || drop(store.accounts().await.unwrap())).await;
        let users_only = median(async || drop(store.users().await.unwrap())).await;
        let accounts = store.accounts().await.unwrap();
        let people = ductus_runtime::alerts::people(&accounts.users, &accounts.former);
        let start = Instant::now();
        let mut reached = 0;
        for _ in 0..5 {
            for t in &texts {
                let reach = owners::reach(t, &people, &teams);
                if owners::advice(t, &reach, &people).is_some() || reach.ok() {
                    reached += 1;
                }
                if ductus_core::alerts::owner_addresses(t, &people, &teams).is_some() {
                    reached += 1;
                }
            }
        }
        let matching = start.elapsed() / 5;
        println!(
            "{label}: {} accounts, {} former names; read users {users_only:?}, users + former names {read:?}; \
             match {} owners (reach + advice + addresses) {matching:?} ({reached} hits)",
            accounts.users.len(),
            accounts.former.len(),
            texts.len()
        );
    };
    report("no former names").await;

    // Three renames each (display name twice, email once): the owners' texts are all former now.
    for (i, id) in ids.iter().enumerate() {
        for (display, email) in
            [(format!("Owner {i:03} b"), None), (format!("Owner {i:03} c"), Some(format!("owner{i:03}.c@example.org")))]
        {
            let change = UserChange { display_name: Some(display), email, ..Default::default() };
            store.update_user(*id, &change, &audit()).await.unwrap();
        }
    }
    report("three former names each").await;
    people.forget().await.unwrap();
}
