//! A scalar driver follows a linear target on each input interval. The unclipped
//! followed value feeds children; only the lag relative to the target drives art.

use serde::{Deserialize, Serialize};

use crate::{NodeId, ParamId, SlotId};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct MotionResponse {
    pub inputs: Vec<ResponseInput>,
    pub period_s: f32,
    pub damping_ratio: f32,
    pub output_param: Option<ParamId>,
}

impl Default for MotionResponse {
    fn default() -> Self {
        Self {
            inputs: Vec::new(),
            period_s: 0.4,
            damping_ratio: 0.7,
            output_param: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ResponseInput {
    pub source: ResponseSource,
    pub gain: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ResponseSource {
    PoseParam {
        param: ParamId,
    },
    PartSlot {
        part: NodeId,
        slot: SlotId,
        angle_deg: f32,
        reference_length: f32,
    },
    Parent,
}

#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct SpringState {
    pub x: f64,
    pub velocity: f64,
}

impl SpringState {
    pub fn advance(&mut self, start: f64, end: f64, dt: f64, omega: f64, damping: f64) {
        // In coordinates relative to a linearly moving target, the forcing
        // vanishes. Integrate the homogeneous damped oscillator in closed form.
        let slope = (end - start) / dt;
        let e = self.x - start;
        let u = self.velocity - slope;
        let a = omega * damping;
        let (c, s) = if damping < 1.0 {
            let b = omega * (1.0 - damping * damping).sqrt();
            let decay = (-a * dt).exp();
            (decay * (b * dt).cos(), decay * (b * dt).sin() / b)
        } else if damping == 1.0 {
            let decay = (-a * dt).exp();
            (decay, dt * decay)
        } else {
            let b = omega * (damping * damping - 1.0).sqrt();
            let slow = (-omega / (damping + b / omega) * dt).exp();
            let fast = (-(a + b) * dt).exp();
            (
                (slow + fast) * 0.5,
                slow * -(-2.0 * b * dt).exp_m1() / (2.0 * b),
            )
        };
        self.x = end + c * e + s * (u + a * e);
        self.velocity = slope + c * u - s * (a * u + omega * omega * e);
    }
}
