//! Threading: messages grouped into conversations, each reply after what it answers, newest
//! activity first.

use std::collections::HashMap;

use super::*;

// ---- threading

/// One message's place in a thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// Index into the messages threaded.
    pub msg: usize,
    /// How far it is indented: 0 for the thread's start (and siblings with no common parent here).
    pub depth: u32,
    /// Another copy of a message already in the mailbox (same Message-ID).
    pub dup: bool,
    /// It answers a message that isn't in this mailbox.
    pub orphan: bool,
}

/// A conversation: its messages in reading order (each reply after what it answers, siblings by
/// date), and when it began and was last active.
#[derive(Debug, Clone)]
pub struct Thread {
    pub items: Vec<Item>,
    pub first: Option<i64>,
    pub last: Option<i64>,
    /// The earliest-read message: its key names the thread, and stays the same as more is read.
    pub first_read: usize,
    pub(super) newest: usize,
}

pub(super) struct Node {
    pub(super) msg: Option<usize>,
    pub(super) parent: Option<usize>,
    pub(super) children: Vec<usize>,
}

pub(super) struct Arena {
    pub(super) nodes: Vec<Node>,
}

impl Arena {
    pub(super) fn add(&mut self, msg: Option<usize>) -> usize {
        self.nodes.push(Node {
            msg,
            parent: None,
            children: Vec::new(),
        });
        self.nodes.len() - 1
    }

    /// Whether `a` is `b` or one of its ancestors: linking `a` below `b` would make a loop. A
    /// chain too long to walk counts as a loop, so no link is made.
    pub(super) fn above(&self, a: usize, b: usize) -> bool {
        if a == b {
            return true;
        }
        if self.nodes[a].children.is_empty() {
            return false; // a leaf is nobody's ancestor
        }
        let mut at = self.nodes[b].parent;
        for _ in 0..MAX_WALK {
            match at {
                None => return false,
                Some(p) if p == a => return true,
                Some(p) => at = self.nodes[p].parent,
            }
        }
        true
    }

    pub(super) fn link(&mut self, parent: usize, child: usize) {
        self.nodes[child].parent = Some(parent);
        self.nodes[parent].children.push(child);
    }

    pub(super) fn date(&self, msgs: &[Head], n: usize) -> Option<i64> {
        self.nodes[n].msg.and_then(|m| msgs[m].date)
    }

    /// The earliest and latest date below `top` (itself included).
    pub(super) fn span(&self, msgs: &[Head], top: usize) -> (Option<i64>, Option<i64>) {
        let (mut first, mut last) = (None::<i64>, None::<i64>);
        let mut stack = vec![top];
        while let Some(n) = stack.pop() {
            if let Some(d) = self.date(msgs, n) {
                first = Some(first.map_or(d, |f| f.min(d)));
                last = Some(last.map_or(d, |l| l.max(d)));
            }
            stack.extend_from_slice(&self.nodes[n].children);
        }
        (first, last)
    }

    pub(super) fn unlink(&mut self, child: usize) {
        if let Some(p) = self.nodes[child].parent.take() {
            let c = &mut self.nodes[p].children;
            if let Some(i) = c.iter().position(|&x| x == child) {
                c.remove(i);
            }
        }
    }
}

/// A thread's top, for joining by subject.
pub(super) struct Top {
    pub(super) node: usize,
    pub(super) key: String,
    pub(super) reply: bool,
    pub(super) first: Option<i64>,
    pub(super) last: Option<i64>,
}

/// Threads of `msgs`, newest activity first. Every message is in exactly one thread.
pub fn thread(msgs: &[Head]) -> Vec<Thread> {
    let mut a = Arena {
        nodes: Vec::with_capacity(msgs.len() + msgs.len() / 2),
    };
    let mut by_id: HashMap<u64, usize> = HashMap::with_capacity(msgs.len());
    let mut dup = vec![false; msgs.len()];
    let mut dups = Vec::new(); // (a duplicate's node, its original's node)

    // 1. a node per Message-ID; References link each reply below what it answers
    for (i, m) in msgs.iter().enumerate() {
        let me = match m.id {
            Some(id) => match by_id.get(&id).copied() {
                Some(n) if a.nodes[n].msg.is_none() => {
                    a.nodes[n].msg = Some(i);
                    n
                }
                Some(n) => {
                    dup[i] = true;
                    let d = a.add(Some(i));
                    dups.push((d, n));
                    d
                }
                None => {
                    let n = a.add(Some(i));
                    by_id.insert(id, n);
                    n
                }
            },
            None => a.add(Some(i)),
        };
        let mut prev: Option<usize> = None;
        for &r in m.refs.iter().filter(|&&r| Some(r) != m.id) {
            let n = match by_id.get(&r) {
                Some(&n) => n,
                None => {
                    let n = a.add(None);
                    by_id.insert(r, n);
                    n
                }
            };
            // what other messages said stands; only a missing link is added, never a loop
            if let Some(p) = prev
                && a.nodes[n].parent.is_none()
                && !a.above(n, p)
            {
                a.link(p, n);
            }
            prev = Some(n);
        }
        // the message itself knows best what it replies to: its last reference
        let parent = prev.filter(|&p| !a.above(me, p));
        if a.nodes[me].parent != parent {
            a.unlink(me);
            if let Some(p) = parent {
                a.link(p, me);
            }
        }
    }
    // a duplicate that doesn't say what it answers goes next to its original
    for (d, n) in dups {
        if a.nodes[d].parent.is_none() && !a.above(d, n) {
            a.link(n, d);
        }
    }
    let orphan: Vec<bool> = msgs
        .iter()
        .map(|m| {
            m.refs
                .iter()
                .rev()
                .find(|&&r| Some(r) != m.id)
                .is_some_and(|r| by_id.get(r).is_none_or(|&n| a.nodes[n].msg.is_none()))
        })
        .collect();

    // 2 and 3. the roots; placeholders go, their children taking their place (not at the top
    // level, where a placeholder with several children stays to hold them together)
    let roots: Vec<usize> = (0..a.nodes.len())
        .filter(|&n| a.nodes[n].parent.is_none())
        .collect();
    let mut order = Vec::with_capacity(a.nodes.len());
    let mut stack = roots.clone();
    while let Some(n) = stack.pop() {
        order.push(n);
        stack.extend_from_slice(&a.nodes[n].children);
    }
    for &n in order.iter().rev() {
        // children before parents: each child's own children are pruned already
        let kids = std::mem::take(&mut a.nodes[n].children);
        let mut keep = Vec::with_capacity(kids.len());
        for c in kids {
            if a.nodes[c].msg.is_some() {
                keep.push(c);
                continue;
            }
            let grand = std::mem::take(&mut a.nodes[c].children);
            for &g in &grand {
                a.nodes[g].parent = Some(n);
            }
            keep.extend(grand);
            a.nodes[c].parent = None;
        }
        a.nodes[n].children = keep;
    }
    let mut tops = Vec::new();
    for r in roots {
        let n = &a.nodes[r];
        if n.msg.is_some() || n.children.len() > 1 {
            tops.push(r);
        } else if let [c] = n.children[..] {
            a.nodes[r].children.clear();
            a.nodes[c].parent = None;
            tops.push(c);
        }
    }

    // 4. what's still apart: a reply joins the conversation with its subject that was active
    // shortly before it
    let mut tops: Vec<Top> = tops
        .into_iter()
        .map(|node| {
            // a placeholder's subject is its earliest child's
            let msg = a.nodes[node].msg.or_else(|| {
                a.nodes[node]
                    .children
                    .iter()
                    .filter_map(|&c| a.nodes[c].msg)
                    .min_by_key(|&m| (msgs[m].date.is_none(), msgs[m].date, m))
            });
            let (key, reply) = msg.map_or((String::new(), false), |m| {
                (
                    subject_key(&msgs[m].subject),
                    strip_subject(&msgs[m].subject, true).1,
                )
            });
            let (first, last) = a.span(msgs, node);
            Top {
                node,
                key,
                reply,
                first,
                last,
            }
        })
        .collect();
    let mut by_subject: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, t) in tops.iter().enumerate() {
        if !t.key.is_empty() {
            by_subject.entry(t.key.as_str()).or_default().push(i);
        }
    }
    let mut groups: Vec<Vec<usize>> = by_subject.into_values().filter(|g| g.len() > 1).collect();
    groups.sort(); // the same result every time
    let mut gone = vec![false; tops.len()];
    for mut g in groups {
        g.sort_by_key(|&i| (tops[i].first.is_none(), tops[i].first, tops[i].node));
        let mut anchor: Option<usize> = None;
        for i in g {
            let near = anchor.is_some_and(|k| {
                matches!((tops[k].last, tops[i].first), (Some(l), Some(f)) if f - l <= SUBJECT_WINDOW)
            });
            match anchor {
                Some(k) if tops[i].reply && near => {
                    let joined = join(&mut a, &tops[k], tops[i].node);
                    tops[k].node = joined;
                    tops[k].last = tops[k].last.max(tops[i].last);
                    gone[i] = true;
                }
                _ => anchor = Some(i),
            }
        }
    }

    // 5. each thread in reading order
    let mut out = Vec::new();
    for (t, _) in tops.iter().zip(&gone).filter(|(_, g)| !**g) {
        let mut items = Vec::new();
        let mut stack = vec![(t.node, 0u32)];
        while let Some((n, depth)) = stack.pop() {
            let below = match a.nodes[n].msg {
                Some(m) => {
                    items.push(Item {
                        msg: m,
                        depth,
                        dup: dup[m],
                        orphan: orphan[m],
                    });
                    depth + 1
                }
                None => depth,
            };
            let mut kids = a.nodes[n].children.clone();
            kids.sort_by_key(|&c| {
                let m = a.nodes[c].msg;
                (a.date(msgs, c).unwrap_or(i64::MAX), m.unwrap_or(usize::MAX))
            });
            stack.extend(kids.into_iter().rev().map(|c| (c, below)));
        }
        let (first, last) = a.span(msgs, t.node);
        let first_read = items.iter().map(|x| x.msg).min().unwrap_or(0);
        let newest = items.iter().map(|x| x.msg).max().unwrap_or(0);
        out.push(Thread {
            items,
            first,
            last,
            first_read,
            newest,
        });
    }
    out.sort_by(|x, y| y.last.cmp(&x.last).then(y.newest.cmp(&x.newest)));
    out
}

/// Joins the thread `r` (its top node) to the conversation `k`, returning `k`'s top node: below
/// the original message when `k` starts with one, else side by side under a placeholder.
pub(super) fn join(a: &mut Arena, k: &Top, r: usize) -> usize {
    let moved = if a.nodes[r].msg.is_some() {
        vec![r]
    } else {
        std::mem::take(&mut a.nodes[r].children)
    };
    let top = if a.nodes[k.node].msg.is_none() || !k.reply {
        k.node
    } else {
        let d = a.add(None);
        a.link(d, k.node);
        d
    };
    for c in moved {
        a.link(top, c);
    }
    top
}
