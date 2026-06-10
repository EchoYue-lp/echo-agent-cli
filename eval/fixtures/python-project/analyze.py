"""Eval fixture: Python data analysis script with intentional bugs."""

import csv
import sys
from collections import defaultdict


def load_csv(filepath):
    """Load CSV file and return list of dicts."""
    rows = []
    with open(filepath, "r") as f:
        reader = csv.DictReader(f)
        for row in reader:
            rows.append(row)
    return rows


def calculate_mean(values):
    """Calculate mean of a list of numbers.
    BUG: does not handle empty list (ZeroDivisionError).
    """
    total = sum(values)
    return total / len(values)


def group_by(data, key):
    """Group rows by a key column."""
    groups = defaultdict(list)
    for row in data:
        groups[row[key]].append(row)
    return dict(groups)


def survival_rate_by_class(data):
    """Calculate survival rate per passenger class."""
    groups = group_by(data, "pclass")
    result = {}
    for cls, rows in groups.items():
        survived = sum(1 for r in rows if r["survived"] == "1")
        total = len(rows)
        result[cls] = round(survived / total, 4) if total > 0 else 0.0
    return result


if __name__ == "__main__":
    filepath = sys.argv[1] if len(sys.argv) > 1 else "titanic-small.csv"
    data = load_csv(filepath)
    print(f"Loaded {len(data)} rows, {len(data[0])} columns")

    # BUG: calculate_mean called with string values (age column not converted to float)
    ages = [row["age"] for row in data]
    mean_age = calculate_mean(ages)  # This will crash: can't sum strings
    print(f"Mean age: {mean_age}")

    rates = survival_rate_by_class(data)
    print("Survival rate by class:")
    for cls, rate in sorted(rates.items()):
        print(f"  Class {cls}: {rate:.1%}")
