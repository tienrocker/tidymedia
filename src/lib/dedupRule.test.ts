import { describe, expect, it } from "vitest";
import { groupCheckState, rangeIds, ruleChecked, RuleMember } from "./dedupRule";

function m(
  fileId: number,
  patch: Partial<RuleMember> = {},
): RuleMember {
  return {
    fileId,
    size: 1000,
    mtime: 1_000_000,
    status: 0,
    width: 100,
    height: 100,
    takenAt: null,
    ...patch,
  };
}

describe("ruleChecked", () => {
  it("giu dung 1 ban o moi nhom - tick hang loat khong bao gio xoa sach nhom", () => {
    const groups: RuleMember[][] = [
      [m(1), m(2, { width: 200, height: 200 })],
      [m(3), m(4), m(5)],
      [m(6, { size: 50 }), m(7, { size: 900 })],
    ];
    for (const g of groups) {
      for (const rule of ["res", "oldest", "newest"] as const) {
        const marked = ruleChecked(g, rule);
        expect(marked.size).toBe(g.length - 1);
        expect(marked.size).toBeLessThan(g.length);
      }
    }
  });

  it("res giu ban do phan giai cao nhat, hoa thi giu ban nang hon", () => {
    const marked = ruleChecked([m(1), m(2, { width: 400, height: 300 })], "res");
    expect([...marked]).toEqual([1]);
    const tie = ruleChecked([m(1, { size: 10 }), m(2, { size: 20 })], "res");
    expect([...tie]).toEqual([1]);
  });

  it("oldest/newest dung takenAt truoc, khong co thi mtime", () => {
    const a = m(1, { takenAt: 100 });
    const b = m(2, { takenAt: 500 });
    expect([...ruleChecked([a, b], "oldest")]).toEqual([2]);
    expect([...ruleChecked([a, b], "newest")]).toEqual([1]);
    const byMtime = [m(1, { mtime: 10 }), m(2, { mtime: 20 })];
    expect([...ruleChecked(byMtime, "oldest")]).toEqual([2]);
  });

  it("file khong con present khong bao gio duoc chon lam ban giu", () => {
    const missing = m(1, { status: 1, width: 4000, height: 3000 });
    const present = m(2);
    expect([...ruleChecked([missing, present], "res")]).toEqual([1]);
  });

  it("nhom 1 ban (con lai da mat) thi khong danh dau gi", () => {
    expect(ruleChecked([m(1)], "res").size).toBe(0);
    expect(ruleChecked([], "res").size).toBe(0);
  });
});

describe("groupCheckState", () => {
  it("suy tu (so ban da danh dau, so ban cua nhom)", () => {
    expect(groupCheckState(0, 3)).toBe("none");
    expect(groupCheckState(1, 3)).toBe("partial");
    expect(groupCheckState(2, 3)).toBe("all");
    // Danh dau ca ban da mat (get_dup_group tra ca status != 0) van la "all"
    expect(groupCheckState(3, 3)).toBe("all");
  });
});

describe("rangeIds (Shift+click kieu Gmail)", () => {
  const ordered = [10, 20, 30, 40, 50];

  it("lay ca dai giua anchor va o vua bam, ca 2 chieu", () => {
    expect(rangeIds(20, 40, ordered)).toEqual([20, 30, 40]);
    expect(rangeIds(40, 20, ordered)).toEqual([20, 30, 40]);
  });

  it("chua co anchor thi chi minh o do", () => {
    expect(rangeIds(null, 30, ordered)).toEqual([30]);
  });

  it("anchor khong con trong list (vua reload) thi chi minh o do", () => {
    expect(rangeIds(99, 30, ordered)).toEqual([30]);
  });

  it("o khong co trong list thi khong dong gi ca", () => {
    expect(rangeIds(20, 99, ordered)).toEqual([]);
  });

  it("anchor trung o vua bam = 1 phan tu", () => {
    expect(rangeIds(30, 30, ordered)).toEqual([30]);
  });
});
