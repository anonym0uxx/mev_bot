use pump_quant_evaluator::walk_forward::*;

#[test]
fn clean_expanding_window_passes() {
    let folds = [
        Fold::new(100, 101, 200),
        Fold::new(200, 201, 300),
        Fold::new(300, 301, 400),
    ];
    assert_eq!(assert_chronological(&folds), Ok(()));
}

#[test]
fn test_touching_train_is_lookahead() {
    // test_start == train_end violates strict post-dating.
    let folds = [Fold::new(100, 100, 200)];
    assert_eq!(
        assert_chronological(&folds),
        Err(Leak::TestOverlapsTrain { fold: 0 })
    );
    // test_start < train_end likewise.
    let folds = [Fold::new(100, 90, 200)];
    assert_eq!(
        assert_chronological(&folds),
        Err(Leak::TestOverlapsTrain { fold: 0 })
    );
}

#[test]
fn malformed_window_detected() {
    // test_start > test_end. Second fold is the offender (index 1).
    let folds = [Fold::new(100, 101, 200), Fold::new(200, 260, 250)];
    assert_eq!(
        assert_chronological(&folds),
        Err(Leak::MalformedWindow { fold: 1 })
    );
}

#[test]
fn folds_out_of_order_detected() {
    // Third fold's train_end (150) regresses below the second (200).
    let folds = [
        Fold::new(100, 101, 200),
        Fold::new(200, 201, 300),
        Fold::new(150, 301, 400),
    ];
    assert_eq!(
        assert_chronological(&folds),
        Err(Leak::FoldsOutOfOrder { fold: 2 })
    );
}

#[test]
fn empty_and_single_fold_are_trivially_ok() {
    assert_eq!(assert_chronological(&[]), Ok(()));
    assert_eq!(assert_chronological(&[Fold::new(10, 11, 20)]), Ok(()));
}

#[test]
fn first_violation_in_order_wins() {
    // Fold 0 is fine; fold 1 both overlaps train AND the set is out of order —
    // the per-fold look-ahead check fires first and reports fold 1.
    let folds = [Fold::new(100, 101, 200), Fold::new(50, 50, 60)];
    assert_eq!(
        assert_chronological(&folds),
        Err(Leak::TestOverlapsTrain { fold: 1 })
    );
}
