//! 事件循环的 Rust 侧状态:虚拟时钟 + 定时器调度表 + 待发网络请求队列。
//!
//! 虚拟时钟从 0 开始;没有就绪任务、没有在途网络时,时钟直接快进到下一个
//! 定时器的触发点——`setTimeout(5000)` 不耗真实时间,整次执行可复现。
//! 回调本体活在 JS 侧(bootstrap 的 Map),这里只有 id 与时间,不跨 GC 持引用。

use std::collections::BTreeMap;

use crate::net::HttpRequest;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimerId(pub u32);

#[derive(Debug)]
pub struct Timer {
    pub id: TimerId,
    /// setInterval 的重复间隔;setTimeout 为 None
    pub interval_ms: Option<u64>,
}

#[derive(Default)]
pub struct EventLoopState {
    /// 虚拟时钟,毫秒。起点 0(确定性:不读墙钟)。
    pub now_ms: u64,
    next_timer_id: u32,
    next_seq: u64,
    /// (fire_at, seq) → timer。BTreeMap 保证同刻定时器按登记序触发。
    timers: BTreeMap<(u64, u64), Timer>,
    /// fetch op 排进来、settle 循环取走的请求。(request_id, spec)
    pending_requests: Vec<(u32, HttpRequest)>,
    next_request_id: u32,
}

impl EventLoopState {
    pub fn schedule_timer(&mut self, delay_ms: u64, repeating: bool) -> TimerId {
        self.next_timer_id += 1;
        let id = TimerId(self.next_timer_id);
        self.insert_timer(
            Timer {
                id,
                interval_ms: repeating.then_some(delay_ms.max(1)),
            },
            self.now_ms + delay_ms,
        );
        id
    }

    fn insert_timer(&mut self, timer: Timer, fire_at: u64) {
        self.next_seq += 1;
        self.timers.insert((fire_at, self.next_seq), timer);
    }

    pub fn clear_timer(&mut self, id: TimerId) {
        self.timers.retain(|_, t| t.id != id);
    }

    /// 到点的第一个定时器。interval 会以当前虚拟时刻为基准重新入队。
    pub fn pop_ready_timer(&mut self) -> Option<TimerId> {
        let (&(fire_at, seq), _) = self.timers.first_key_value()?;
        if fire_at > self.now_ms {
            return None;
        }
        let timer = self.timers.remove(&(fire_at, seq))?;
        let id = timer.id;
        if let Some(interval) = timer.interval_ms {
            let next_at = self.now_ms + interval;
            self.insert_timer(timer, next_at);
        }
        Some(id)
    }

    /// 下一个定时器的触发时刻(虚拟毫秒)。
    pub fn next_timer_at(&self) -> Option<u64> {
        self.timers.first_key_value().map(|(&(at, _), _)| at)
    }

    pub fn timers_remaining(&self) -> usize {
        self.timers.len()
    }

    pub fn queue_request(&mut self, req: HttpRequest) -> u32 {
        self.next_request_id += 1;
        self.pending_requests.push((self.next_request_id, req));
        self.next_request_id
    }

    pub fn take_requests(&mut self) -> Vec<(u32, HttpRequest)> {
        std::mem::take(&mut self.pending_requests)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timer_ordering_and_fast_forward() {
        let mut el = EventLoopState::default();
        let a = el.schedule_timer(100, false);
        let b = el.schedule_timer(50, false);
        let c = el.schedule_timer(100, false); // 与 a 同刻,登记序在后

        assert_eq!(el.pop_ready_timer(), None);
        assert_eq!(el.next_timer_at(), Some(50));
        el.now_ms = 50;
        assert_eq!(el.pop_ready_timer(), Some(b));
        assert_eq!(el.pop_ready_timer(), None);
        el.now_ms = 100;
        assert_eq!(el.pop_ready_timer(), Some(a));
        assert_eq!(el.pop_ready_timer(), Some(c));
        assert_eq!(el.pop_ready_timer(), None);
        assert_eq!(el.timers_remaining(), 0);
    }

    #[test]
    fn interval_reschedules_and_clears() {
        let mut el = EventLoopState::default();
        let i = el.schedule_timer(10, true);
        el.now_ms = 10;
        assert_eq!(el.pop_ready_timer(), Some(i));
        assert_eq!(el.next_timer_at(), Some(20));
        el.clear_timer(i);
        assert_eq!(el.timers_remaining(), 0);
    }

    #[test]
    fn zero_delay_interval_still_advances() {
        let mut el = EventLoopState::default();
        el.schedule_timer(0, true);
        el.now_ms = 0;
        assert!(el.pop_ready_timer().is_some());
        // interval 至少 1ms,时钟必须能前进,否则永不 settle 也烧不出预算
        assert_eq!(el.next_timer_at(), Some(1));
    }
}
