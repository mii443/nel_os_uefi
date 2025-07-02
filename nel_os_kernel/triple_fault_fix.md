# Triple Fault 修正内容

## 問題の概要
`paging::initialize()`関数を実行するとQEMUがトリプルフォルトで落ちる問題が発生していました。

## 原因
1. **カーネルの仮想アドレスを物理アドレスとして使用**
   - 静的変数のページテーブルのアドレスをそのまま物理アドレスとして扱っていた
   - カーネルは0x100000付近の仮想アドレスで動作しているが、これは物理アドレスではない

2. **物理アドレス0x0の使用**
   - ビットマップメモリアロケータが最初の1MBを予約していなかった
   - 物理アドレス0x0にページテーブルを配置しようとしてトリプルフォルト

3. **不十分なメモリマッピング範囲**
   - 最初は4GBまでしかマッピングしていなかった
   - ヒープが5GB付近に配置されたためページフォルトが発生

## 修正内容

### 1. ビットマップメモリアロケータの修正 (memory.rs)
```rust
// 最初の1MB（256ページ）を予約
const RESERVED_PAGES: usize = 256; // 1MB / 4KB

let mut table = Self {
    used_map,
    start: RESERVED_PAGES,  // 0ではなく256から開始
    end: usize::MAX,
};

// 最初の1MBを明示的に予約済みとしてマーク
for i in 0..RESERVED_PAGES {
    table.set_frame(i, false);
}
```

### 2. ページテーブル初期化の修正 (paging.rs)
```rust
// 静的変数を削除し、動的に物理メモリを確保
pub unsafe fn initialize(bitmap_table: &mut BitmapMemoryTable) -> PhysFrame {
    let new_frame = unsafe { initialize_identity_mapping(bitmap_table) };
    // ...
}

unsafe fn initialize_identity_mapping(bitmap_table: &mut BitmapMemoryTable) -> PhysFrame {
    // 物理フレームを動的に確保
    let pml4_frame = bitmap_table.allocate_frame().expect("Failed to allocate PML4 frame");
    let pml4_addr = pml4_frame.start_address().as_u64();
    let pml4_table = unsafe { &mut *(pml4_addr as *mut PageTable) };
    *pml4_table = PageTable::new();
    
    // PDPテーブルも同様に確保
    let pdp_frame = bitmap_table.allocate_frame().expect("Failed to allocate PDP frame");
    // ...
}
```

### 3. メモリマッピング範囲の拡張
```rust
// 4GBから64GBへ拡張
// Create identity mapping for first 64GB (64 PDP entries, each covering 1GB)
for i in 0..64 {
    // 各1GBエントリに対してページディレクトリを作成
    // ...
}
```

### 4. main.rsの修正
```rust
// bitmap_tableをpaging::initialize()に渡す
let mut mapper = {
    unsafe { paging::initialize(&mut bitmap_table) };
    let lv4_table = get_active_level_4_table();
    unsafe { OffsetPageTable::new(lv4_table, VirtAddr::new(0x0)) }
};
```

### 5. unsafe操作の適切な処理
```rust
// unsafe操作をunsafeブロックで囲む
let new_frame = unsafe { initialize_identity_mapping(bitmap_table) };

// raw constを使用してmutable staticへの参照を作成
PML4_TABLE[0].set_frame(phys_frame(&raw const PDP_TABLE), flags);
```

## 結果
- 物理メモリアロケータが正しく0x208000から物理フレームを割り当てるようになった
- 64GBまでの恒等マッピングにより、5GB付近のヒープアクセスも成功
- トリプルフォルトが解消され、カーネルが正常に動作するようになった

## 学んだこと
1. カーネルの仮想アドレスと物理アドレスを区別することの重要性
2. 低位メモリ（0-1MB）は予約領域として扱うべき
3. ページテーブルのマッピング範囲は使用するメモリ全体をカバーする必要がある
4. UEFIは恒等マッピングを提供しているため、物理アドレスへの直接アクセスが可能