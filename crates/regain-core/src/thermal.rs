//! Shared bounded camera cooler policy. Vendor transports encode the output.
// Host-controller policy, not camera registers or copied SDK PID constants:
// proportional percentage points/degree C and integral points/(degree C*s).
pub const PROPORTIONAL: f64 = 8.0;
const INTEGRAL: f64 = 0.12;

pub fn recovery_state(
    power: f64,
    prior: f64,
    current: f64,
    target: f64,
    previous_target: f64,
) -> (f64, f64) {
    if target > previous_target {
        return (0.0, 0.0);
    }
    let integral = (power - PROPORTIONAL * (prior - previous_target)).clamp(0.0, 100.0);
    let boosted = (power + PROPORTIONAL * (current - prior + previous_target - target).max(0.0))
        .clamp(0.0, 100.0);
    (boosted, integral)
}

pub fn target_change(
    power: f64,
    integral: f64,
    temperature: f64,
    previous: f64,
    target: f64,
) -> (f64, f64) {
    if temperature <= target {
        return (0.0, 0.0);
    }
    let demand = (integral + PROPORTIONAL * (temperature - target)).clamp(0.0, 100.0);
    // Recompute proportional demand immediately for a deliberate setpoint
    // change. The caller applies the live power-change limit before writing it.
    (
        if target < previous {
            power.max(demand)
        } else {
            power.min(demand)
        },
        integral,
    )
}

// Percentage points per five seconds. Do not bank ramp allowance across a
// blocked USB operation; at most two seconds may be applied in one update.
pub fn limit_power(power: f64, demand: f64, elapsed: f64, maximum: f64) -> f64 {
    let change = maximum / 5.0 * elapsed.clamp(0.0, 2.0);
    demand
        .clamp(0.0, 100.0)
        .clamp((power - change).max(0.0), (power + change).min(100.0))
}

pub fn regulate(power: f64, integral: &mut f64, error: f64, elapsed: f64, maximum: f64) -> f64 {
    let dt = elapsed.clamp(0.0, 2.0);
    let candidate = (*integral + error * INTEGRAL * dt).clamp(0.0, 100.0);
    let demand = candidate + PROPORTIONAL * error;
    // Do not accumulate demand the actuator cannot deliver. Allow unwinding.
    if (demand <= 100.0 || error < 0.0) && (demand >= 0.0 || error > 0.0) {
        *integral = candidate;
    }
    limit_power(power, *integral + PROPORTIONAL * error, dt, maximum)
}
