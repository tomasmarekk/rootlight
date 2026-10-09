module test
  ! <- module
  !     ^ variable
  type, private, abstract :: abstract_type
    ! <- type
    !    ^ attribute
    !             ^ attribute
    !                         ^ type
  end type abstract_type
  ! <- keyword
  !         ^ type
  type, public, extends(abstract_type) :: concrete_type
    ! <- type
    !    ^ attribute
    !             ^ keyword
    !                         ^ type
  end type concrete_type
end module
