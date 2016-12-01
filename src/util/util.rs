#![feature(plugin, core_intrinsics, const_fn)]

extern crate libc;
extern crate bsdlike_getopts as getopts;
extern crate memmap;
use memmap::Mmap;

#[macro_use]
extern crate macros;

use std::mem::{size_of, uninitialized, transmute, replace, forget};
use std::ptr;
use std::ptr::copy;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::default::Default;
use std::num::ParseIntError;
use std::cmp::max;
use std::slice;
use std::fmt::{Debug, Display, Formatter};
use std::borrow::{Cow, Borrow, BorrowMut};
use std::ops::{Deref, DerefMut, Index, IndexMut, Range, RangeFrom, RangeTo, RangeFull, Add};
use std::cell::{UnsafeCell, Cell};
use std::marker::PhantomData;
use std::hash::{Hash, BuildHasherDefault};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io;

extern crate fnv;
use fnv::FnvHasher;
extern crate nodrop;
use nodrop::NoDrop;

pub use Endian::*;
//use std::ty::Unsafe;

mod trivial_hasher;
pub use trivial_hasher::*;

mod small_vector;
pub use small_vector::SmallVector;

pub struct ReadCell<T: Copy> {
    pub value: UnsafeCell<T>
}
impl<T: Copy> ReadCell<T> {
    pub const fn new(value: T) -> Self {
        ReadCell { value: UnsafeCell::new(value) }
    }
    pub fn get(&self) -> T {
        unsafe { *self.value.get() }
    }
}

pub unsafe trait ROSlicePtr {
    fn as_ptr(&self) -> *const u8;
    fn len(&self) -> usize;
}
pub unsafe trait RWSlicePtr<'a>: Sized {
    fn as_mut_ptr(self) -> *mut u8;
    fn len(&self) -> usize;
    #[inline]
    fn set_memory(self, byte: u8) {
        let len = self.len();
        unsafe { memset(self.as_mut_ptr(), byte as i32, len); }
    }
}
macro_rules! impl_rosp { ($ty:ty) => {
    unsafe impl ROSlicePtr for $ty {
        #[inline(always)]
        fn as_ptr(&self) -> *const u8 {
            unsafe { transmute(self.as_ptr()) }
        }
        #[inline(always)]
        fn len(&self) -> usize { self.len() }
    }
} }
macro_rules! impl_rwsp { ($ty:ty) => {
    unsafe impl<'a> RWSlicePtr<'a> for $ty {
        #[inline(always)]
        fn as_mut_ptr(self) -> *mut u8 {
            unsafe { transmute(self.as_ptr()) }
        }
        #[inline(always)]
        fn len(&self) -> usize { (**self).len() }
    }
} }

impl_rosp!([u8]);
impl_rosp!([Cell<u8>]);
impl_rosp!([ReadCell<u8>]);
impl_rosp!([i8]);

impl_rwsp!(&'a [Cell<u8>]);
impl_rwsp!(&'a mut [u8]);

#[inline]
pub fn copy_from_slice<'a, T: Copy + Swap, S: ?Sized + ROSlicePtr>(slice: &S, end: Endian) -> T {
    assert_eq!(slice.len(), size_of::<T>());
    unsafe {
        let mut t : T = uninitialized();
        copy(transmute(slice.as_ptr()), &mut t, 1);
        t.bswap_from(end);
        t
    }
}

#[inline]
pub fn copy_to_slice<'a, T: Copy + Swap, S: RWSlicePtr<'a>>(slice: S, t: &T, end: Endian) {
    assert_eq!(slice.len(), size_of::<T>());
    unsafe {
        let stp: *mut T = transmute(slice.as_mut_ptr());
        copy(t, stp, 1);
        (*stp).bswap_from(end);
    }
}

pub fn copy_to_vec<T: Copy + Swap>(vec: &mut Vec<u8>, t: &T, end: Endian) {
    let size = size_of::<T>();
    let off = vec.len();
    assert!(off <= !0usize - size);
    unsafe {
        vec.reserve(size);
        vec.set_len(off + size);
        let stp: *mut T = transmute(vec.as_mut_ptr().offset(off as isize));
        copy(t, stp, 1);
        (*stp).bswap_from(end);
    }
}

pub fn copy_to_new_vec<T: Swap>(t: &T, end: Endian) -> Vec<u8> {
    unsafe {
        let mut res: Vec<u8> = slice::from_raw_parts(transmute(t), size_of::<T>()).to_vec();
        let newt: *mut T = transmute(res.as_mut_ptr());
        (*newt).bswap_from(end);
        res
    }
}

#[derive(Clone, Copy)]
pub struct Unswapped<T: Swap> {
    pub unswapped: T,
}
impl<T: Swap> Swap for Unswapped<T> {
    #[inline]
    fn bswap(&mut self) {}
}

impl<T: Swap> Unswapped<T> {
    pub fn copy(&self, endian: Endian) -> T {
        let mut result = self.unswapped;
        result.bswap_from(endian);
        result
    }
}

pub trait Is { type T; }
impl<T> Is for T { type T = T; }

pub trait Cast<Other>: Sized {
    type SelfBase;
    type OtherBase;
    fn _len(&self) -> usize;
    unsafe fn raw_cast(self) -> Other;

    #[inline]
    fn cast_to_u8(self) -> Other where Self::OtherBase: Is<T=u8> {
        unsafe { self.raw_cast() }
    }
    fn cast<U>(self) -> (Other, usize /*slack*/) where U: Swap, Self::OtherBase: Is<T=Unswapped<U>> {
        let len = self._len();
        (unsafe { self.raw_cast() },
         len * size_of::<Self::SelfBase>() % size_of::<Self::OtherBase>())
    }
}

impl<'a, T, U> Cast<&'a [U]> for &'a [T] where T: Swap {
    type SelfBase = T;
    type OtherBase = U;
    fn _len(&self) -> usize { self.len() }
    #[inline]
    unsafe fn raw_cast(self) -> &'a [U] {
        let len = self.len();
        slice::from_raw_parts(
            transmute(self.as_ptr()),
            len * size_of::<T>() / size_of::<U>()
        )
    }
}
impl<'a, T, U> Cast<&'a mut [U]> for &'a mut [T] where T: Swap {
    type SelfBase = T;
    type OtherBase = U;
    fn _len(&self) -> usize { self.len() }
    #[inline]
    unsafe fn raw_cast(self) -> &'a mut [U] {
        let len = self.len();
        slice::from_raw_parts_mut(
            transmute(self.as_ptr()),
            len * size_of::<T>() / size_of::<U>()
        )
    }
}
impl<T, U> Cast<Mem<U>> for Mem<T> where T: Swap, U: Copy {
    type SelfBase = T;
    type OtherBase = U;
    fn _len(&self) -> usize { self.len }
    #[inline]
    unsafe fn raw_cast(self) -> Mem<U> {
        Mem {
            mc: self.mc,
            ptr: transmute(self.ptr),
            len: (self.len * size_of::<T>()) / size_of::<U>(),
        }
    }
}

impl<T, U> Cast<Vec<U>> for Vec<T> {
    type SelfBase = T;
    type OtherBase = U;
    fn _len(&self) -> usize { self.len() }
    #[inline]
    unsafe fn raw_cast(self) -> Vec<U> {
        let (ptr, len, cap) = (self.as_ptr(), self.len(), self.capacity());
        let (sizeof_t, sizeof_u) = (size_of::<T>(), size_of::<U>());
        if cap * sizeof_t % sizeof_u != 0 {
            panic!("vec raw cast can't have slack since allocation might get wonky")
        }
        forget(self);
        Vec::from_raw_parts(transmute(ptr),
                            (len * sizeof_t) / sizeof_u,
                            (cap * sizeof_t) / sizeof_u)
    }
}


#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub enum Endian {
    BigEndian,
    LittleEndian,
}

impl Default for Endian {
    fn default() -> Endian { BigEndian }
}

impl Endian {
    #[inline(always)]
    pub fn needs_swap(self) -> bool {
        self == BigEndian
    }
}

pub trait Swap: Copy {
    fn bswap(&mut self);
    #[inline]
    fn bswap_from(&mut self, end: Endian) {
        if end == BigEndian { self.bswap() }
    }
}

pub trait CheckAdd<Other, DummyForCoherence> {
    type Output;
    fn check_add(self, other: Other) -> Option<Self::Output>;
}
pub trait CheckSub<Other, DummyForCoherence> {
    type Output;
    fn check_sub(self, other: Other) -> Option<Self::Output>;
}
pub trait CheckMul<Other, DummyForCoherence> {
    type Output;
    fn check_mul(self, other: Other) -> Option<Self::Output>;
}


macro_rules! impl_int {($ty:ident) => {
    impl Swap for $ty {
        #[inline]
        fn bswap(&mut self) {
            *self = self.swap_bytes();
        }
    }
    impl IntStuff for $ty {
        fn from_str_radix(src: &str, radix: u32) -> Result<$ty, ParseIntError> {
            $ty::from_str_radix(src, radix)
        }
        fn align_to(self, size: $ty) -> $ty {
            let mask = size - 1;
            (self + mask) & !mask
        }
    }
    impl CheckAdd<$ty, $ty> for $ty {
        type Output = $ty;
        #[inline]
        fn check_add(self, other: $ty) -> Option<Self::Output> {
            self.checked_add(other)
        }
    }
    impl CheckSub<$ty, $ty> for $ty {
        type Output = $ty;
        #[inline]
        fn check_sub(self, other: $ty) -> Option<Self::Output> {
            self.checked_sub(other)
        }
    }
    impl CheckMul<$ty, $ty> for $ty {
        type Output = $ty;
        #[inline]
        fn check_mul(self, other: $ty) -> Option<Self::Output> {
            self.checked_mul(other)
        }
    }
    impl_check_x_option!(CheckAdd, check_add, $ty, $ty);
    impl_check_x_option!(CheckSub, check_sub, $ty, $ty);
    impl_check_x_option!(CheckMul, check_mul, $ty, $ty);
}}


macro_rules! impl_signed {($ty:ident) => {
    impl_int!($ty);
    impl IntStuffSU for $ty {
        fn neg_if_possible(self) -> Option<Self> { Some(-self) }
    }
}}
macro_rules! impl_unsigned {($ty:ident) => {
    impl_int!($ty);
    impl IntStuffSU for $ty {
        fn neg_if_possible(self) -> Option<Self> { None }
    }
    impl SignExtend for $ty {
        fn sign_extend(self, bits: u8) -> Self {
            self | ((0 as $ty).wrapping_sub((self >> (bits - 1)) & 1) << bits)
        }
        fn un_sign_extend(self, bits: u8) -> Option<Self> {
            let masked = self & ((1 << bits) - 1);
            if masked.sign_extend(bits) == self { Some(masked) } else { None }
        }
    }
}}

impl_unsigned!(usize);
impl_signed!(isize);
impl_unsigned!(u64);
impl_signed!(i64);
impl_unsigned!(u32);
impl_signed!(i32);
impl_unsigned!(u16);
impl_signed!(i16);

pub trait Ext<Larger> {
    fn ext(self) -> Larger;
}
pub trait Narrow<Smaller> {
    fn trunc(self) -> Smaller;
    fn narrow(self) -> Option<Smaller>;
}

macro_rules! impl_unsigned_unsigned {($sm:ident, $la:ident) => {
    impl Ext<$la> for $sm {
        fn ext(self) -> $la {
            self as $la
        }
    }
    impl Narrow<$sm> for $la {
        fn trunc(self) -> $sm {
            self as $sm
        }
        fn narrow(self) -> Option<$sm> {
            let res = self as $sm;
            if res as $la == self { Some(res) } else { None }
        }
    }
}}

impl_unsigned_unsigned!(usize, u64);
impl_unsigned_unsigned!(u32, u64);
impl_unsigned_unsigned!(u16, u64);
impl_unsigned_unsigned!(u8, u64);
impl_unsigned_unsigned!(u32, usize);
impl_unsigned_unsigned!(u16, usize);
impl_unsigned_unsigned!(u8, usize);
impl_unsigned_unsigned!(u16, u32);
impl_unsigned_unsigned!(u8, u32);
impl_unsigned_unsigned!(u8, u16);


impl Swap for u8 {
    fn bswap(&mut self) {}
}
impl Swap for i8 {
    fn bswap(&mut self) {}
}
impl<T> Swap for *mut T {
    fn bswap(&mut self) {
        let xself: &mut usize = unsafe { transmute(self) };
        xself.bswap()
    }
}
impl<T> Swap for *const T {
    fn bswap(&mut self) {
        let xself: &mut usize = unsafe { transmute(self) };
        xself.bswap()
    }
}

impl<A: Swap, B: Swap> Swap for (A, B) {
    fn bswap(&mut self) {
        self.0.bswap();
        self.1.bswap();
    }
}

// dumb
macro_rules! impl_for_array{($cnt:expr) => (
    impl<T: Swap> Swap for [T; $cnt] {
        fn bswap(&mut self) {}
    }
)}
impl_for_array!(1);
impl_for_array!(2);
impl_for_array!(3);
impl_for_array!(4);
impl_for_array!(8);
impl_for_array!(16);
impl<T: Swap> Swap for Option<T> {
    fn bswap(&mut self) {}
}

#[derive(PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ByteStr([u8]);
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct ByteString(pub Vec<u8>);
impl ByteStr {
    #[inline]
    pub fn lossy<'a>(&'a self) -> Cow<'a, str> {
        String::from_utf8_lossy(&self.0)
    }
    #[inline]
    pub fn from_bytes(s: &[u8]) -> &ByteStr {
        unsafe { transmute(s) }
    }
    #[inline]
    pub fn from_str(s: &str) -> &ByteStr {
        ByteStr::from_bytes(s.as_bytes())
    }
    #[inline]
    pub fn from_bytes_mut(s: &mut [u8]) -> &mut ByteStr {
        unsafe { transmute(s) }
    }
    pub fn find(&self, pat: u8) -> Option<usize> {
        let ptr = self.0.as_ptr();
        let chr = unsafe { memchr(ptr, pat as i32, self.0.len()) };
        if chr == 0 as *mut u8 {
            None
        } else {
            Some((chr as usize) - (ptr as usize))
        }
    }
    /*
    pub fn rfind<P>(&self, pat: P) -> Option<usize>
        where for<'b> P: Pattern<'b>,
              for<'b> <P as Pattern<'b>>::Searcher: ReverseSearcher<'b>
              */
    pub fn rfind(&self, pat: u8) -> Option<usize> {
        for i in (0..self.len()).rev() {
            if self[i] == pat {
                return Some(i);
            }
        }
        None
    }
    pub fn find_bstr(&self, pat: &ByteStr) -> Option<usize> {
        let res = unsafe { memmem(self.as_ptr(), self.len(),
                                  pat.as_ptr(), pat.len()) };
        if res == 0 as *mut u8 {
            None
        } else {
            Some((res as usize) - (self.as_ptr() as usize))
        }
    }
    pub fn unix_basename(&self) -> &ByteStr {
        if let Some(pos) = self.rfind(b'/') { &self[pos+1..] } else { &self[..] }
    }
}
pub trait SomeRange<T> {}
impl<T> SomeRange<T> for RangeTo<T> {}
impl<T> SomeRange<T> for RangeFrom<T> {}
impl<T> SomeRange<T> for Range<T> {}
impl<T> SomeRange<T> for RangeFull {}
impl<T> Index<T> for ByteStr
    where T: SomeRange<usize>, [u8]: Index<T, Output=[u8]> {
    type Output = ByteStr;
    #[inline]
    fn index(&self, idx: T) -> &Self::Output {
        ByteStr::from_bytes(&self.0[idx])
    }
}
impl<T> IndexMut<T> for ByteStr
    where T: SomeRange<usize>, [u8]: IndexMut<T, Output=[u8]> {
    #[inline]
    fn index_mut(&mut self, idx: T) -> &mut Self::Output {
        ByteStr::from_bytes_mut(&mut self.0[idx])
    }
}
impl Index<usize> for ByteStr {
    type Output = u8;
    #[inline]
    fn index(&self, idx: usize) -> &u8 {
        &self.0[idx]
    }
}
impl IndexMut<usize> for ByteStr {
    #[inline]
    fn index_mut(&mut self, idx: usize) -> &mut u8 {
        &mut self.0[idx]
    }
}

impl ByteString {
    pub fn new(s: &ByteStr) -> Self {
        ByteString(s.0.to_owned())
    }
    pub fn from_bytes(s: &[u8]) -> Self {
        ByteString(s.to_owned())
    }
    pub fn from_str(s: &str) -> ByteString {
        ByteString::from_bytes(s.as_bytes())
    }
    pub fn from_vec(s: Vec<u8>) -> ByteString {
        ByteString(s)
    }
    pub fn from_string(s: String) -> ByteString {
        ByteString(s.into_bytes())
    }
    pub fn with_capacity(c: usize) -> ByteString {
        ByteString(Vec::with_capacity(c))
    }
    pub fn concat2(left: &ByteStr, right: &ByteStr) -> ByteString {
        let mut result = ByteString::with_capacity(left.len() + right.len());
        result.push_bstr(left);
        result.push_bstr(right);
        result

    }
    pub fn push_bstr(&mut self, bs: &ByteStr) {
        self.0.extend_from_slice(&bs.0);
    }
}
impl Deref for ByteString {
    type Target = ByteStr;
    fn deref(&self) -> &ByteStr { unsafe { transmute::<&[u8], &ByteStr>(&self.0[..]) } }
}
impl DerefMut for ByteString {
    fn deref_mut(&mut self) -> &mut ByteStr { unsafe { transmute::<&mut [u8], &mut ByteStr>(&mut self.0[..]) } }
}
impl Deref for ByteStr {
    type Target = [u8];
    fn deref(&self) -> &[u8] { &self.0 }
}
impl DerefMut for ByteStr {
    fn deref_mut(&mut self) -> &mut [u8] { &mut self.0 }
}
impl Debug for ByteStr {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        Debug::fmt(&self.lossy()[..], f)
    }
}
impl Display for ByteStr {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        Display::fmt(&self.lossy()[..], f)
    }
}
impl Debug for ByteString {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        Debug::fmt(&**self, f)
    }
}
impl Display for ByteString {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        Display::fmt(&**self, f)
    }
}
impl Borrow<ByteStr> for ByteString {
    #[inline]
    fn borrow(&self) -> &ByteStr {
        &**self
    }
}
impl BorrowMut<ByteStr> for ByteString {
    #[inline]
    fn borrow_mut(&mut self) -> &mut ByteStr {
        &mut **self
    }
}
impl ToOwned for ByteStr {
    type Owned = ByteString;
    fn to_owned(&self) -> ByteString {
        ByteString(self.0.to_owned())
    }
}
impl<'a> From<&'a ByteStr> for Cow<'a, ByteStr> {
    fn from(s: &'a ByteStr) -> Self {
        Cow::Borrowed(s)
    }
}
impl<'a> From<ByteString> for Cow<'a, ByteStr> {
    fn from(s: ByteString) -> Self {
        Cow::Owned(s)
    }
}
impl<'a> From<&'a str> for &'a ByteStr {
    fn from(s: &'a str) -> Self {
        ByteStr::from_bytes(s.as_bytes())
    }
}
impl<'a> Add<&'a ByteStr> for ByteString {
    type Output = ByteString;
    fn add(mut self, other: &ByteStr) -> ByteString {
        self.push_bstr(other);
        self
    }
}

impl PartialEq<str> for ByteStr {
    fn eq(&self, other: &str) -> bool {
        &**self == other.as_bytes()
    }
}
impl PartialEq<ByteStr> for ByteString {
    fn eq(&self, other: &ByteStr) -> bool {
        &***self == &**other
    }
}
impl PartialEq<str> for ByteString {
    fn eq(&self, other: &str) -> bool {
        &***self == other.as_bytes()
    }
}

#[inline]
pub fn from_cstr<'a, S: ?Sized + ROSlicePtr>(chs: &S) -> &'a ByteStr {
    let len = unsafe { strnlen(chs.as_ptr(), chs.len()) };
    unsafe { ByteStr::from_bytes(std::slice::from_raw_parts(chs.as_ptr(), len)) }
}

#[inline]
pub fn from_cstr_strict<'a, S: ?Sized + ROSlicePtr>(chs: &S) -> Option<&'a ByteStr> {
    let len = unsafe { strnlen(chs.as_ptr(), chs.len()) };
    if len == chs.len() {
        None
    } else {
        unsafe { Some(ByteStr::from_bytes(std::slice::from_raw_parts(chs.as_ptr(), len))) }
    }
}


enum MemoryContainer {
    Empty,
    MemoryMap(Mmap),
    BoxedSlice(Box<[u8]>),
}

#[derive(Clone)]
pub struct Mem<T: Copy> {
    mc: Arc<MemoryContainer>,
    ptr: *const T,
    len: usize
}

unsafe impl<T: Copy> Send for Mem<T> {}
unsafe impl<T: Copy> Sync for Mem<T> {}

impl<T: Copy> std::default::Default for Mem<T> {
    fn default() -> Self {
        Mem::empty()
    }
}

impl<T: Copy> Debug for Mem<T> {
    fn fmt(&self, fmt: &mut Formatter) -> std::fmt::Result {
        write!(fmt, "Mem<u8>({:?}, {})", self.ptr, self.len)
    }
}

struct XArcInner<T: ?Sized> {
    strong: AtomicUsize,
    _weak: AtomicUsize,
    _data: T,
}

static EMPTY_ARC_INNER: XArcInner<MemoryContainer> = XArcInner {
    strong: AtomicUsize::new(1),
    _weak: AtomicUsize::new(1),
    _data: MemoryContainer::Empty,
};

impl Mem<u8> {
    pub fn with_mm(mm: Mmap) -> Self {
        let (ptr, len) = (mm.ptr() as *const _, mm.len());
        Mem {
            mc: Arc::new(MemoryContainer::MemoryMap(mm)),
            ptr: ptr, len: len
        }
    }
}

impl<T: Copy> Mem<T> {
    pub fn with_data(data: &[T]) -> Self where T: Clone {
        Mem::with_vec(data.to_owned())
    }

    pub fn with_vec(vec: Vec<T>) -> Self {
        let len = vec.len();
        let vec: Vec<u8> = unsafe { vec.raw_cast() };
        let bs = vec.into_boxed_slice();
        let ptr = bs.as_ptr();
        Mem {
            mc: Arc::new(MemoryContainer::BoxedSlice(bs)),
            ptr: unsafe { transmute(ptr) }, len: len
        }
    }

    #[inline]
    pub fn empty() -> Self {
        let old_size = EMPTY_ARC_INNER.strong.fetch_add(1, Ordering::Relaxed);
        if old_size > std::isize::MAX as usize {
            unsafe { std::intrinsics::abort(); }
        }
        Mem {
            mc: unsafe { transmute(&EMPTY_ARC_INNER) },
            ptr: 0 as *const T,
            len: 0,
        }
    }

    pub fn into_vec(mut self) -> Vec<T> {
        if let Some(mc) = Arc::get_mut(&mut self.mc) {
            let ok = if let &mut MemoryContainer::BoxedSlice(ref bs) = mc {
                bs.as_ptr() == (self.ptr as *const u8) && bs.len() == self.len * size_of::<T>()
            } else { false };
            if ok {
                if let MemoryContainer::BoxedSlice(bs) = replace(mc, MemoryContainer::Empty) {
                    return unsafe { bs.into_vec().raw_cast() };
                } else { debug_assert!(false); }
            }
        }
        fast_slice_to_owned(self.get())
    }

    pub fn slice(&self, from: usize, to: usize) -> Option<Self> {
        let len = to - from;
        if from > self.len || len > self.len - from {
            return None
        }
        unsafe {
            Some(Mem { mc: self.mc.clone(), ptr: self.ptr.offset(from as isize), len: len })
        }
    }

    pub fn get(&self) -> &[T] {
        unsafe { std::slice::from_raw_parts::<T>(self.ptr, self.len) }
    }

    pub fn get_mut(&mut self) -> Option<&mut [T]> {
        if let Some(mc) = Arc::get_mut(&mut self.mc) {
            if let &mut MemoryContainer::BoxedSlice(_) = mc {
                unsafe { return Some(std::slice::from_raw_parts_mut::<T>(self.ptr as *mut T, self.len)); }
            }
        }
        None
    }

    pub fn get_mut_decow(&mut self) -> &mut [T] {
        if let Some(sl) = self.get_mut() {
            // sl is already &mut [T], but this fudges the lifetimes.  This shouldn't be necessary,
            // but since both mutable borrows (this one and the else branch) have the same lifetime
            // as self, rustc thinks the borrows conflict.
            let sl: &mut [T] = unsafe { transmute(sl) };
            return sl;
        }
        *self = Mem::with_vec(self.get().to_owned());
        self.get_mut().unwrap()
    }

    // only safe to call if there are no mutable references
    pub unsafe fn get_cells(&self) -> &[Cell<T>] {
        transmute(std::slice::from_raw_parts(self.ptr, self.len))
    }

    pub fn offset_in(&self, other: &Mem<u8>) -> Option<usize> {
        let mine = self.ptr as usize;
        let theirs = other.ptr as usize;
        if mine >= theirs && mine < theirs + max(other.len, 1) {
            Some(mine - theirs)
        } else { None }
    }
    pub fn len(&self) -> usize {
        self.len
    }
}

pub fn memmap(fil: &File) -> io::Result<Mem<u8>> {
    Ok(Mem::with_mm(try!(Mmap::open(fil, memmap::Protection::ReadCopy))))
}

pub fn do_getopts(args: &[String], min_expected_free: usize, max_expected_free: usize, optgrps: &mut Vec<getopts::OptGroup>) -> Option<getopts::Matches> {
    if let Ok(m) = getopts::getopts(args, &optgrps) {
        if m.free.len() >= min_expected_free &&
            m.free.len() <= max_expected_free {
            return Some(m);
        }
    }
    None
}

pub fn do_getopts_or_usage(args: &[String], top: &str, min_expected_free: usize, max_expected_free: usize, optgrps: &mut Vec<getopts::OptGroup>) -> Result<getopts::Matches, String> {
    do_getopts(args, min_expected_free, max_expected_free, optgrps).ok_or_else(|| { usage(top, optgrps) })
}

pub fn usage(top: &str, optgrps: &mut Vec<getopts::OptGroup>) -> String {
    optgrps.push(getopts::optflag("h", "help", "This help"));
    getopts::usage(top, &optgrps)
}

pub fn exit() -> ! {
    unsafe { libc::exit(1) }
}

fn isprint(c: char) -> bool {
    let c = c as u32;
    if c >= 32 { c < 127 } else { (1 << c) & 0x3e00 != 0 }
}

fn shell_safe(c: char) -> bool {
    match c {
        'a' ... 'z' | 'A' ... 'Z' | '0' ... '9' |
        '_' | '\\' | '.' | '@' | '/' | '+' | '-'
          => true,
        _ => false
    }
}

pub fn shell_quote(args: &[String]) -> String {
    let mut sb = std::string::String::new();
    for arg_ in args.iter() {
        let arg = &arg_[..];
        if sb.len() != 0 { sb.push(' ') }
        if arg.chars().all(shell_safe) {
            sb.push_str(arg);
        } else {
            sb.push('"');
            for ch_ in arg.as_bytes().iter() {
                let ch = *ch_ as char;
                if ch == '$' || ch == '`' || ch == '\\' || ch == '"' || ch == '\n' {
                    if ch == '\n' {
                        sb.push_str("\\n");
                    } else {
                        sb.push('\\');
                        sb.push(ch);
                    }
                } else if !isprint(ch) {
                    sb.push_str(&format!("\\\\x{:02x}", *ch_));
                } else {
                    sb.push(ch);
                }
            }
            sb.push('"');
        }
    }
    sb
}


pub trait OptionExt<T> {
    fn and_tup<U>(self, other: Option<U>) -> Option<(T, U)>;
    fn is_some_and<P>(&self, pred: P) -> bool
        where P: FnOnce(&T) -> bool;
}
impl<T> OptionExt<T> for Option<T> {
    fn and_tup<U>(self, other: Option<U>) -> Option<(T, U)> {
        if let Some(s) = self {
            if let Some(o) = other {
                return Some((s, o));
            }
        }
        None
    }
    fn is_some_and<P>(&self, pred: P) -> bool
        where P: FnOnce(&T) -> bool {
        if let &Some(ref val) = self {
            pred(val)
        } else {
            false
        }
    }
}

pub trait SliceExt<T> {
    fn slice_opt(&self, start: usize, end: usize) -> Option<&[T]>;
}
impl<T> SliceExt<T> for [T] {
    fn slice_opt(&self, start: usize, end: usize) -> Option<&[T]> {
        let len = self.len();
        if end > len || start > end {
            None
        } else {
            unsafe { Some(std::slice::from_raw_parts(self.as_ptr().offset(start as isize), end - start)) }
        }
    }
}

pub trait VecCopyExt<T> {
    fn extend_slice(&mut self, other: &[T]);
}

impl<T: Copy> VecCopyExt<T> for Vec<T> {
    fn extend_slice(&mut self, other: &[T]) {
        unsafe {
            let ol = other.len();
            let sl = self.len();
            self.reserve(ol);
            self.set_len(sl + ol);
            copy(other.as_ptr(), self.as_mut_ptr().offset(sl as isize), ol);
        }
    }
}

pub trait VecStrExt {
    fn strings(&self) -> Vec<String>;
}
impl<T: std::string::ToString> VecStrExt for Vec<T> {
    fn strings(&self) -> Vec<String> { self.iter().map(|x| x.to_string()).collect() }
}

pub trait IntStuffSU : Sized {
    fn neg_if_possible(self) -> Option<Self>;
}

pub trait IntStuff : IntStuffSU {
    fn from_str_radix(src: &str, radix: u32) -> Result<Self, ParseIntError>;
    fn align_to(self, size: Self) -> Self;
}

pub trait SignExtend : Sized {
    fn sign_extend(self, bits: u8) -> Self;
    fn un_sign_extend(self, bits: u8) -> Option<Self>;
}

pub fn stoi<T: IntStuff>(mut s: &str) -> Option<T> {
    if s == "" { return None; }
    let neg = &s[..1] == "-";
    if neg { s = &s[1..]; }
    let mut base = 10;
    if s.len() > 2 && &s[2..3] != "-" {
        let prefix = &s[..2];
        if prefix == "0x" {
            base = 16;
            s = &s[2..];
        } else if prefix == "0b" {
            base = 2;
            s = &s[2..];
        } else if prefix == "0o" {
            base = 8;
            s = &s[2..];
        }
    }
    let result = IntStuff::from_str_radix(s, base);
    let mut result = result.ok();
    if neg { result = result.and_then(|x: T| x.neg_if_possible()); }
    result
}

#[derive(Debug, Clone)]
pub struct GenericError(pub String);
display_as_debug!(GenericError);
impl std::error::Error for GenericError {
    fn description(&self) -> &str { &*self.0 }
}

#[test]
fn test_branch() {
    let do_i = |i: usize| {
        branch!(if (i == 1) {
            // Due to rustc being a piece of shit, ... I don't even.  You can only have one `let` (or any expression-as-statement), so make it count.  Maybe tomorrow I will figure this out.  Such a waste of time...
            type A = isize;
            type B = isize;
            let (b, c) = (7usize, 8)
        } else {
            type A = usize;
            type B = usize;
            let (b, c) = (8usize, 9)
        } then {
            println!("{}", (b + c) as A);
        })
    };
    for i in 0..2 {
        do_i(i)
    }
}

extern {
    fn memmove(dst: *mut u8, src: *const u8, len: usize);
    fn memset(dst: *mut u8, byte: i32, len: usize);
    fn memchr(src: *const u8, byte: i32, len: usize) -> *mut u8;
    fn memmem(big: *const u8, big_len: usize, little: *const u8, little_len: usize) -> *mut u8;
}
#[inline(always)]
unsafe fn strnlen(s: *const u8, maxlen: usize) -> usize {
    mod orig {
        extern { pub fn strnlen(s: *const u8, maxlen: usize) -> usize; }
    }
    let res = orig::strnlen(s, maxlen);
    std::intrinsics::assume(res <= maxlen);
    res
}
#[inline]
pub fn copy_memory(src: &[u8], dst: &mut [u8]) {
    assert_eq!(dst.len(), src.len());
    unsafe { memmove(dst.as_mut_ptr(), src.as_ptr(), dst.len()); }
}

pub fn into_cow<'a, T: ?Sized + ToOwned, S: Into<Cow<'a, T>>>(s: S) -> Cow<'a, T> {
    s.into()
}

pub struct Lazy<T> {
    mtx: Mutex<()>,
    val: UnsafeCell<NoDrop<T>>,
    is_valid: AtomicBool,
}
impl<T> Lazy<T> {
    pub fn new() -> Lazy<T> {
        Lazy {
            mtx: Mutex::new(()),
            val: unsafe { UnsafeCell::new(NoDrop::new(uninitialized())) },
            is_valid: AtomicBool::new(false),
        }
    }
    pub fn get<F>(&self, f: F) -> &T where F: FnOnce() -> T {
        unsafe {
            if !self.is_valid.load(Ordering::Acquire) {
                let _guard = self.mtx.lock().unwrap();
                ptr::write(&mut **self.val.get(), f());
                self.is_valid.store(true, Ordering::Release);
            }
            &*self.val.get()
        }
    }
}
unsafe impl<T> Sync for Lazy<T> {}
impl<T> Drop for Lazy<T> {
    fn drop(&mut self) {
        if self.is_valid.load(Ordering::Acquire) {
            unsafe { ptr::read(&mut **self.val.get()); }
        }
    }
}

pub struct FieldLens<Outer, Inner> {
    offset: usize,
    lol: PhantomData<fn(&Outer)->&Inner>,
}
impl<O, I> Clone for FieldLens<O, I> {
    fn clone(&self) -> Self { *self }
}
impl<O, I> Copy for FieldLens<O, I> {}

pub unsafe fn __field_lens<Outer, Inner>(offset: *const Inner) -> FieldLens<Outer, Inner> {
    FieldLens { offset: offset as usize, lol: PhantomData }
}

impl<Outer, Inner> FieldLens<Outer, Inner> {
    #[inline]
    pub fn get_mut(&self, outer: &mut Outer) -> &mut Inner {
        unsafe { transmute(transmute::<&mut Outer, *mut u8>(outer).offset(self.offset as isize)) }
    }
    #[inline]
    pub fn get(&self, outer: &Outer) -> &Inner {
        unsafe { transmute(transmute::<&Outer, *const u8>(outer).offset(self.offset as isize)) }
    }
    #[inline]
    pub unsafe fn get_mut_unsafe(&self, outer: *mut Outer) -> &mut Inner {
        transmute(transmute::<*mut Outer, *mut u8>(outer).offset(self.offset as isize))
    }
}

pub type Fnv = BuildHasherDefault<FnvHasher>;

pub fn new_fnv_hashmap<K: Eq + Hash, V>() -> HashMap<K, V, Fnv> {
    HashMap::with_hasher(Fnv::default())
}

pub fn new_fnv_hashset<T: Eq + Hash>() -> HashSet<T, Fnv> {
    HashSet::with_hasher(Fnv::default())
}

#[cfg(stopwatch)]
thread_local!(static STOPWATCH_INDENT: Cell<usize> = Cell::new(0));
#[cfg(stopwatch)]
use std::time::Instant;

#[cfg(not(stopwatch))]
pub struct Stopwatch<'a>(PhantomData<&'a str>);
#[cfg(stopwatch)]
pub struct Stopwatch<'a> {
    desc: &'a str,
    start_time: Instant,
    indent: usize,
}
#[cfg(stopwatch)]
pub fn stopwatch(desc: &str) -> Stopwatch {
    let indent = STOPWATCH_INDENT.with(|cell| {
        let indent = cell.get();
        cell.set(indent + 4);
        indent
    });
    Stopwatch { desc: desc, start_time: Instant::now(), indent: indent }
}
#[cfg(not(stopwatch))]
pub fn stopwatch(_desc: &str) -> Stopwatch { Stopwatch(PhantomData) }

impl<'a> Stopwatch<'a> {
    pub fn stop(self) {}
}
#[cfg(stopwatch)]
impl<'a> Drop for Stopwatch<'a> {
    fn drop(&mut self) {
        let duration = Instant::now() - self.start_time;
        println!("{blank:spaces$}{desc}: {duration:?}", blank="", spaces=self.indent,
                 desc=self.desc, duration=duration);
        STOPWATCH_INDENT.with(|cell| cell.set(self.indent));
    }
}

#[inline(always)]
pub fn empty_slice<T>() -> &'static [T] {
    unsafe { std::slice::from_raw_parts(!0 as *const T, 0) }
}

pub fn fast_slice_to_owned<T: Copy>(slice: &[T]) -> Vec<T> {
    // extend_from_slice is supposed to be fast.  in unoptimized mode, it takes 60s to copy a few hundred MB.
    let len = slice.len();
    let mut res = Vec::with_capacity(len);
    unsafe { res.set_len(len); }
    res.copy_from_slice(slice);
    res

}
