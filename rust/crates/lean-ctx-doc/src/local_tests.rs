// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::model::{Page, PageStatus};
use crate::{sections, write};

fn ids(scope: &str) -> impl FnMut(&str) -> String {
    let scope = scope.replace('/', "-");
    let mut n = 0;
    move |p: &str| {
        n += 1;
        format!("{p}_{scope}_{n:04}")
    }
}

fn create(store: &LocalDocs, who: &Author, at: &str, slug: &str, md: &str) -> Committed {
    let mut new_id = ids(slug);
    store
        .commit::<LocalError>(who, at, "write", |_| {
            let (title, secs) = sections::split(md);
            let page = Page {
                slug: slug.into(),
                title,
                kind: "guide".into(),
                status: PageStatus::Draft,
                sections: secs,
                anchors: Vec::new(),
                notes: Vec::new(),
            };
            Ok(write::create_page(&page, &[], &mut new_id).0)
        })
        .expect("create")
}

#[test]
fn revisions_keep_history_and_every_earlier_state() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = LocalDocs::at(dir.path().join("p"));
    assert!(store.entries().expect("empty").is_empty());
    let agent = Author::agent("mcp-1");
    let person = Author::person("Yves");
    let first = create(
        &store,
        &agent,
        "2026-11-04T10:00:00Z",
        "guide/retry",
        "# Retry\n\nOnce.\n",
    );
    assert_eq!(first.rev, Some(1));

    let mut new_id = ids("revise");
    let second = store
        .commit::<LocalError>(&person, "2026-11-04T11:00:00Z", "edit", |space| {
            let page = space.page_by_slug("guide/retry").expect("page");
            let links = space.links.get(&page.page.id).cloned().unwrap_or_default();
            Ok(write::revise_page(
                page,
                "# Retry\n\nTwice.\n",
                &links,
                &[],
                &mut new_id,
            ))
        })
        .expect("revise");
    assert_eq!(second.rev, Some(2));
    let nothing = store
        .commit::<LocalError>(&person, "2026-11-04T12:00:00Z", "edit", |_| Ok(Vec::new()))
        .expect("empty plan");
    assert_eq!(
        nothing,
        Committed {
            rev: None,
            events: 0
        }
    );

    let entries = store.entries().expect("entries");
    let now = space_of(&entries, None, &[]);
    let page = now.page_by_slug("guide/retry").expect("page now");
    assert_eq!(page.markdown(), "# Retry\n\nTwice.\n");
    let then = space_of(&entries, Some(1), &[]);
    assert_eq!(
        then.page_by_slug("guide/retry")
            .expect("page then")
            .markdown(),
        "# Retry\n\nOnce.\n"
    );

    let id = page_id_of(&entries, "guide/retry").expect("id");
    let revs = history(&entries, &id);
    let who: Vec<(u64, &str, &str)> = revs
        .iter()
        .map(|r| (r.rev, r.author.label.as_str(), r.action.as_str()))
        .collect();
    assert_eq!(who, [(1, "mcp-1", "write"), (2, "Yves", "edit")]);
    assert_eq!(revs[1].at, "2026-11-04T11:00:00Z");
    assert_eq!(last_revisions(&entries).get(&id), revs.last());

    let pages = store.pages().expect("pages");
    assert_eq!(pages.len(), 1);
    assert_eq!(pages[0].body(), "# Retry\n\nTwice.\n");
}

#[test]
fn a_torn_append_is_ignored_and_dropped_but_a_bad_line_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = LocalDocs::at(dir.path());
    let who = Author::agent("a");
    create(&store, &who, "2026-11-04T10:00:00Z", "a", "# A\n\nx\n");
    let path = dir.path().join(JOURNAL);
    let mut f = OpenOptions::new().append(true).open(&path).expect("open");
    f.write_all(b"{\"rev\":2,\"se").expect("torn");
    drop(f);
    assert_eq!(store.space().expect("reads").pages.len(), 1);

    create(&store, &who, "2026-11-04T10:01:00Z", "b", "# B\n\ny\n");
    let entries = store.entries().expect("entries after repair");
    assert!(entries.iter().all(|e| e.rev <= 2));
    assert_eq!(store.space().expect("space").pages.len(), 2);

    let raw = fs::read_to_string(&path).expect("raw");
    fs::write(&path, format!("not json\n{raw}")).expect("corrupt");
    assert_eq!(store.entries(), Err(LocalError::Corrupt { line: 1 }));
}
