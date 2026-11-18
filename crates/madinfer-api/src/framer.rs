//! 合帧（Framer）：把后端逐 token 的输出按窗口（缺省 80 ms）合成 Delta，减少跨语言回调次数。
//!
//! 范式同 `TransferThrottle`：距上次放行 ≥ 窗口即放行；否则折叠进 pending 并登记一枚到期票（代数）；票到期时代数不符 = 哑火计数；
//! 终态（complete / failed / cancelled）先强制放行 pending 再透传。与节流器的差别只有一条：pending 是**累加**（文本拼接、token 计数相加、
//! patch 追加）而不是"取最新"。文本边界：后端只在完整 UTF-8 码点处产出 [`TokenPiece::text`]，拼接自然安全。

use std::collections::HashMap;

use madinfer_core_types::RequestId;

/// 缺省合帧窗口。
pub const DEFAULT_FRAME_MS: u64 = 80;

/// 后端产出的一片 token（已在完整码点边界）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TokenPiece {
    /// 文本。
    pub text: String,
    /// token 数。
    pub tokens: u32,
    /// 结构化输出的 JSON Patch 片段（RFC 6902，已序列化）；非结构化为空。
    pub patch: Vec<String>,
}

/// 合帧后的增量（event.v1 delta）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Delta {
    /// 请求。
    pub request: RequestId,
    /// 序号（每请求从 0 递增）。
    pub seq: u32,
    /// 文本。
    pub text: String,
    /// token 数。
    pub tokens: u32,
    /// patch 片段。
    pub patch: Vec<String>,
}

/// 合帧效果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FramerEffect {
    /// 投递一帧。
    Emit(Delta),
    /// 登记到期票。
    ArmTimer {
        /// 请求。
        request: RequestId,
        /// 代数。
        generation: u64,
        /// 到期时刻。
        at_ms: u64,
    },
}

/// 计数台账。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FramerStats {
    /// 收到的 token 片。
    pub offered: u64,
    /// 放行的帧。
    pub emitted: u64,
    /// 被折叠进 pending 的片。
    pub coalesced: u64,
    /// 陈旧票。
    pub stale_ticks: u64,
    /// 终态强制放行次数。
    pub forced_flushes: u64,
}

#[derive(Debug, Default)]
struct Entry {
    next_seq: u32,
    last_emit_ms: Option<u64>,
    pending: Option<TokenPiece>,
    generation: u64,
}

/// 合帧器（每请求独立窗口；非线程安全，收口在投递线程）。
#[derive(Debug)]
pub struct Framer {
    window_ms: u64,
    entries: HashMap<RequestId, Entry>,
    stats: FramerStats,
}

impl Framer {
    /// 指定窗口（16～500 ms 之外按缺省）。
    pub fn new(window_ms: u64) -> Self {
        Self {
            window_ms: if (16..=500).contains(&window_ms) {
                window_ms
            } else {
                DEFAULT_FRAME_MS
            },
            entries: HashMap::new(),
            stats: FramerStats::default(),
        }
    }

    /// 窗口。
    pub fn window_ms(&self) -> u64 {
        self.window_ms
    }

    /// 台账。
    pub fn stats(&self) -> FramerStats {
        self.stats
    }

    /// 活动请求数。
    pub fn active(&self) -> usize {
        self.entries.len()
    }

    /// 一片 token 到达。
    pub fn offer(
        &mut self,
        request: RequestId,
        piece: TokenPiece,
        now_ms: u64,
    ) -> Vec<FramerEffect> {
        self.stats.offered += 1;
        let window = self.window_ms;
        let entry = self.entries.entry(request).or_default();
        let window_open = entry
            .last_emit_ms
            .is_some_and(|last| now_ms.saturating_sub(last) < window);
        if !window_open {
            // 窗口已过（或首片）：直接放行，开新窗口。
            let mut effects = Vec::new();
            if let Some(pending) = entry.pending.take() {
                // 理论上不会有 pending（有 pending 必有到期票且窗口未过）；兜底先放行旧的。
                effects.push(FramerEffect::Emit(Self::frame(request, entry, pending)));
                self.stats.emitted += 1;
            }
            effects.push(FramerEffect::Emit(Self::frame(request, entry, piece)));
            entry.last_emit_ms = Some(now_ms);
            self.stats.emitted += 1;
            return effects;
        }
        // 窗口内：折叠。首次折叠登记到期票。
        self.stats.coalesced += 1;
        let first = entry.pending.is_none();
        match entry.pending.as_mut() {
            Some(p) => {
                p.text.push_str(&piece.text);
                p.tokens = p.tokens.saturating_add(piece.tokens);
                p.patch.extend(piece.patch);
            }
            None => entry.pending = Some(piece),
        }
        if first {
            entry.generation += 1;
            let at_ms = entry.last_emit_ms.unwrap_or(now_ms).saturating_add(window);
            return vec![FramerEffect::ArmTimer {
                request,
                generation: entry.generation,
                at_ms,
            }];
        }
        Vec::new()
    }

    /// 到期票到达。
    pub fn on_window_elapsed(
        &mut self,
        request: RequestId,
        generation: u64,
        now_ms: u64,
    ) -> Vec<FramerEffect> {
        let Some(entry) = self.entries.get_mut(&request) else {
            self.stats.stale_ticks += 1;
            return Vec::new();
        };
        if entry.generation != generation || entry.pending.is_none() {
            self.stats.stale_ticks += 1;
            return Vec::new();
        }
        let Some(pending) = entry.pending.take() else {
            return Vec::new();
        };
        entry.last_emit_ms = Some(now_ms);
        self.stats.emitted += 1;
        vec![FramerEffect::Emit(Self::frame(request, entry, pending))]
    }

    /// 终态：强制放行 pending（若有）并移除请求；返回需要先投递的帧。调用方随后投递 complete / failed。
    pub fn finish(&mut self, request: RequestId) -> Vec<FramerEffect> {
        let Some(mut entry) = self.entries.remove(&request) else {
            return Vec::new();
        };
        entry.generation += 1; // 让在途到期票哑火
        match entry.pending.take() {
            Some(pending) => {
                self.stats.emitted += 1;
                self.stats.forced_flushes += 1;
                vec![FramerEffect::Emit(Self::frame(
                    request, &mut entry, pending,
                ))]
            }
            None => Vec::new(),
        }
    }

    fn frame(request: RequestId, entry: &mut Entry, piece: TokenPiece) -> Delta {
        let seq = entry.next_seq;
        entry.next_seq = entry.next_seq.saturating_add(1);
        Delta {
            request,
            seq,
            text: piece.text,
            tokens: piece.tokens,
            patch: piece.patch,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn piece(text: &str) -> TokenPiece {
        TokenPiece {
            text: text.to_owned(),
            tokens: 1,
            patch: Vec::new(),
        }
    }

    fn emitted(effects: &[FramerEffect]) -> Vec<(u32, String)> {
        effects
            .iter()
            .filter_map(|e| match e {
                FramerEffect::Emit(d) => Some((d.seq, d.text.clone())),
                FramerEffect::ArmTimer { .. } => None,
            })
            .collect()
    }

    #[test]
    fn first_piece_passes_then_window_coalesces() {
        let mut f = Framer::new(80);
        let r = RequestId(1);
        assert_eq!(
            emitted(&f.offer(r, piece("会"), 1000)),
            vec![(0, "会".to_owned())]
        );
        let fx = f.offer(r, piece("议"), 1010);
        assert_eq!(
            fx,
            vec![FramerEffect::ArmTimer {
                request: r,
                generation: 1,
                at_ms: 1080
            }]
        );
        assert!(f.offer(r, piece("纪"), 1020).is_empty(), "同窗口再来只折叠");
        assert!(f.offer(r, piece("要"), 1030).is_empty());
        let fx = f.on_window_elapsed(r, 1, 1080);
        assert_eq!(
            emitted(&fx),
            vec![(1, "议纪要".to_owned())],
            "到期一帧拼接三片"
        );
        assert_eq!(f.stats().coalesced, 3);
        // 新窗口从 1080 起：1100 仍在窗口内 → 折叠并登记代数 2 的票。
        let fx = f.offer(r, piece("："), 1100);
        assert!(matches!(
            fx[0],
            FramerEffect::ArmTimer {
                generation: 2,
                at_ms: 1160,
                ..
            }
        ));
    }

    #[test]
    fn stale_ticket_is_ignored_and_finish_forces_flush() {
        let mut f = Framer::new(80);
        let r = RequestId(2);
        f.offer(r, piece("a"), 0);
        f.offer(r, piece("b"), 10);
        let fx = f.finish(r);
        assert_eq!(
            emitted(&fx),
            vec![(1, "b".to_owned())],
            "终态强制放行 pending"
        );
        assert_eq!(f.stats().forced_flushes, 1);
        assert!(
            f.on_window_elapsed(r, 1, 80).is_empty(),
            "请求已结束，旧票哑火"
        );
        assert_eq!(f.stats().stale_ticks, 1);
        assert_eq!(f.active(), 0);
    }

    #[test]
    fn frames_per_second_is_bounded_by_window() {
        let mut f = Framer::new(100);
        let r = RequestId(3);
        let mut frames = 0;
        let mut armed: Option<(u64, u64)> = None;
        // 1 s 内每 5 ms 一片（200 片），到期票按时触发。
        for i in 0..200u64 {
            let now = i * 5;
            if let Some((generation, at)) = armed {
                if at <= now {
                    frames += emitted(&f.on_window_elapsed(r, generation, at)).len();
                    armed = None;
                }
            }
            for e in f.offer(r, piece("x"), now) {
                match e {
                    FramerEffect::Emit(_) => frames += 1,
                    FramerEffect::ArmTimer {
                        generation, at_ms, ..
                    } => armed = Some((generation, at_ms)),
                }
            }
        }
        frames += emitted(&f.finish(r)).len();
        assert!(frames <= 1 + 1000 / 100 + 1, "1 s 内帧数有界：{frames}");
        assert_eq!(f.stats().offered, 200);
    }
}
