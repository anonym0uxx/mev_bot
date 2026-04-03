//! Kelly criterion sizing for the momentum engine.

/// Compute Kelly-optimal position size in lamports.
///
/// Formula: f* = (p×b - q) / b  where b = avg_win_sol / avg_loss_sol
/// Size = wallet_balance × kelly_fraction × f*
///
/// Returns `None` if inputs are degenerate (negative EV, invalid inputs).
pub fn compute_momentum_kelly_size(
    wallet_balance_lamports: u64,
    win_rate: f64,
    avg_win_sol: f64,
    avg_loss_sol: f64,
    kelly_fraction: f64,
) -> Option<u64> {
    if wallet_balance_lamports == 0 { return None; }
    if !(0.0 < win_rate && win_rate < 1.0) { return None; }
    if avg_win_sol <= 0.0 || avg_loss_sol <= 0.0 { return None; }
    if !(0.0 < kelly_fraction && kelly_fraction <= 1.0) { return None; }

    let p = win_rate;
    let q = 1.0 - p;
    let b = avg_win_sol / avg_loss_sol;
    let kelly_f = (p * b - q) / b;

    if kelly_f <= 0.0 { return None; }

    let size_sol = (wallet_balance_lamports as f64 / 1e9) * kelly_fraction * kelly_f;
    if size_sol <= 0.0 { return None; }

    Some((size_sol * 1_000_000_000.0) as u64)
}

/// Minimal trade record for Kelly input computation.
#[derive(Debug, Clone)]
pub struct MomentumPaperTrade {
    pub net_pnl_sol: f64,
}

/// Parse recent trades and extract Kelly inputs (win_rate, avg_win_sol, avg_loss_sol).
/// Returns None if fewer than 10 trades or no wins/losses in sample.
pub fn compute_momentum_kelly_inputs(
    trades: &[MomentumPaperTrade],
    lookback: usize,
) -> Option<(f64, f64, f64)> {
    if trades.len() < 10 { return None; }

    let recent: Vec<&MomentumPaperTrade> = trades.iter().rev().take(lookback).collect();

    let wins: Vec<f64> = recent.iter()
        .filter_map(|t| if t.net_pnl_sol > 0.0 { Some(t.net_pnl_sol) } else { None })
        .collect();
    let losses: Vec<f64> = recent.iter()
        .filter_map(|t| if t.net_pnl_sol < 0.0 { Some(t.net_pnl_sol.abs()) } else { None })
        .collect();

    if wins.is_empty() || losses.is_empty() { return None; }

    let win_rate = wins.len() as f64 / recent.len() as f64;
    let avg_win = wins.iter().sum::<f64>() / wins.len() as f64;
    let avg_loss = losses.iter().sum::<f64>() / losses.len() as f64;

    Some((win_rate, avg_win, avg_loss))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_kelly_positive_ev() {
        let result = compute_momentum_kelly_size(1_000_000_000, 0.60, 0.03, 0.01, 0.25);
        assert!(result.is_some());
        let size_sol = result.unwrap() as f64 / 1e9;
        assert!(size_sol > 0.05 && size_sol < 0.25, "Expected ~0.117 SOL, got {:.4}", size_sol);
    }

    #[test]
    fn test_kelly_negative_ev() {
        assert!(compute_momentum_kelly_size(1_000_000_000, 0.30, 0.01, 0.03, 0.25).is_none());
    }

    #[test]
    fn test_kelly_inputs_sufficient() {
        let mut trades = Vec::new();
        for i in 0..30 {
            trades.push(MomentumPaperTrade {
                net_pnl_sol: if i % 3 == 0 { -0.005 } else { 0.02 },
            });
        }
        let result = compute_momentum_kelly_inputs(&trades, 30);
        assert!(result.is_some());
    }
}
