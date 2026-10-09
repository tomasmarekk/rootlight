module BoundingBox_Method
  !     ^ definition.module
  implicit none (type, external)

  type, private, abstract :: abstract_type
    !                         ^ definition.class
  end type abstract_type
  type, public, extends(abstract_type) :: concrete_type
    !                         ^ reference.class
    !                                       ^ definition.class
  end type concrete_type

  interface
    module pure subroutine initiate_1(obj)
      !                     ^ definition.interface
      integer, intent(inout) :: obj
    end subroutine initiate_1
  end interface

  interface
    module function constructor_2(anotherobj) result(ans)
      !              ^ definition.interface
      class(concrete_type) :: anotherobj
      !       ^ reference.class
      integer :: Ans
    end function constructor_2
  end interface

contains
  module procedure initiate_1
  !                 ^ reference.implementation
  end procedure initiate_1

  module procedure constructor_2
  !                 ^ reference.implementation
    call initiate_1(ans)
    !     ^ reference.call
  end procedure constructor_2

END MODULE BoundingBox_Method
