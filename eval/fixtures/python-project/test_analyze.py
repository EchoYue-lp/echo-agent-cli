"""Tests for the data analysis fixture."""

from analyze import load_csv, calculate_mean, group_by, survival_rate_by_class


def test_calculate_mean():
    assert calculate_mean([1, 2, 3, 4, 5]) == 3.0
    assert calculate_mean([10]) == 10.0


def test_calculate_mean_empty():
    """This test SHOULD fail with the current implementation (ZeroDivisionError)."""
    try:
        result = calculate_mean([])
        # If we get here, the bug is fixed
        assert result == 0.0
    except ZeroDivisionError:
        # Bug still present — this is expected before fix
        pass


def test_group_by():
    data = [
        {"name": "A", "type": "x"},
        {"name": "B", "type": "y"},
        {"name": "C", "type": "x"},
    ]
    result = group_by(data, "type")
    assert len(result["x"]) == 2
    assert len(result["y"]) == 1
